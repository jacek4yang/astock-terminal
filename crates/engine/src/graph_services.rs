//! Deterministic graph, propagation and market-relationship services.

use std::collections::{HashMap, HashSet, VecDeque};

use astock_core::{Adjust, KlinePeriod, Symbol};
use astock_fundamental::FundamentalClient;
use astock_graph::{
    Edge, Engine as PropagationEngine, Event, GraphStore, ImpactEntry, ImpactReport, Node,
    NodeKind, Relation,
};
use astock_market_data::{DataProvider, MarketData};
use astock_storage::Storage;
use futures::{stream, StreamExt};
use serde_json::{json, Value};

const REL_MAX_LAG: usize = 5;
const REL_BOOT_BLOCK: usize = 10;
const REL_BOOT_REPS: usize = 199;
const REL_MIN_ALIGNED: usize = 60;
const REL_NOTE: &str =
    "相关性不等于因果；小样本与行情风格切换会使相关结构不稳定，必须结合来源、时点和基本面复核";

pub async fn subgraph(
    graph: &GraphStore,
    market: &MarketData,
    fundamental: &FundamentalClient,
    storage: &Storage,
    raw: &str,
    hops: Option<u32>,
) -> Result<Value, String> {
    let query = validate_text(raw, "symbol_or_node", 256)?;
    let hops = hops.unwrap_or(2).clamp(1, 3);
    ensure_company(graph, market, fundamental, storage, &query).await?;
    let result = graph
        .subgraph(&query, hops)
        .await
        .map_err(|error| error.to_string())?;
    Ok(json!({
        "center": query,
        "hops": hops,
        "coverage": if result.edges.is_empty() { "identity_only" } else { "sourced_relations" },
        "coverage_note": if result.edges.is_empty() {
            "已解析公司身份，但尚无经公开来源验证的产业链关系"
        } else {
            "仅展示带来源、有效期和置信度的关系"
        },
        "nodes": result.nodes,
        "edges": result.edges,
    }))
}

#[allow(clippy::too_many_arguments)]
pub async fn as_of(
    graph: &GraphStore,
    market: &MarketData,
    fundamental: &FundamentalClient,
    storage: &Storage,
    business_time: i64,
    knowledge_time: i64,
    symbol_or_node: Option<&str>,
    hops: Option<u32>,
) -> Result<Value, String> {
    validate_time(business_time, "business_time")?;
    validate_time(knowledge_time, "knowledge_time")?;
    let query = symbol_or_node
        .map(|value| validate_text(value, "symbol_or_node", 256))
        .transpose()?;
    if let Some(query) = query.as_deref() {
        ensure_company(graph, market, fundamental, storage, query).await?;
    }
    let mut snapshot = graph
        .graph_as_of(business_time, knowledge_time)
        .await
        .map_err(|error| error.to_string())?;
    let hops = hops.unwrap_or(2).clamp(1, 3);
    let mut center = None;
    if let Some(query) = query.as_deref() {
        let node = graph
            .find_node(query)
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| format!("图谱节点不存在：{query}"))?;
        center = Some(node.id.clone());
        let mut visited = HashSet::from([node.id.clone()]);
        let mut queue = VecDeque::from([(node.id, 0_u32)]);
        while let Some((current, depth)) = queue.pop_front() {
            if depth >= hops {
                continue;
            }
            for edge in &snapshot.edges {
                let next = if edge.src == current {
                    Some(&edge.dst)
                } else if edge.dst == current {
                    Some(&edge.src)
                } else {
                    None
                };
                if let Some(next) = next {
                    if visited.insert(next.clone()) {
                        queue.push_back((next.clone(), depth + 1));
                    }
                }
            }
        }
        snapshot
            .edges
            .retain(|edge| visited.contains(&edge.src) && visited.contains(&edge.dst));
        snapshot.nodes.retain(|node| visited.contains(&node.id));
    }
    let mut value =
        serde_json::to_value(snapshot).map_err(|error| format!("图谱快照序列化失败：{error}"))?;
    if let Some(object) = value.as_object_mut() {
        object.insert("center".into(), json!(center));
        object.insert("hops".into(), json!(hops));
    }
    Ok(value)
}

pub async fn shock(
    graph: &GraphStore,
    subject: &str,
    direction: &str,
    magnitude_pct: Option<f64>,
) -> Result<Value, String> {
    let subject = validate_text(subject, "subject", 256)?;
    let direction = match direction.trim().to_ascii_lowercase().as_str() {
        "up" | "涨" | "上涨" => 1,
        "down" | "跌" | "下跌" => -1,
        other => return Err(format!("direction 只能是 up/down，收到 `{other}`")),
    };
    if magnitude_pct.is_some_and(|value| !value.is_finite() || value.abs() > 10_000.0) {
        return Err("magnitude_pct 必须是有限数且绝对值不超过 10000".into());
    }
    let word = if direction > 0 { "上涨" } else { "下跌" };
    let title = magnitude_pct.map_or_else(
        || format!("{subject}{word}"),
        |value| format!("{subject}{word}{}%", value.abs()),
    );
    let event = Event::new(
        format!("engine-shock-{}", now_secs()),
        "manual_research_scenario",
        title,
        subject,
        magnitude_pct.map(|value| value.abs() / 100.0),
        direction,
        now_secs(),
    );
    let report = PropagationEngine::new(graph.clone())
        .propagate(&event)
        .await
        .map_err(|error| error.to_string())?;
    Ok(impact_report_json(&report))
}

pub async fn relationship(
    market: &MarketData,
    raw_symbols: &[String],
    window_days: Option<u32>,
) -> Result<Value, String> {
    if raw_symbols.len() < 2 || raw_symbols.len() > 12 {
        return Err("symbols 需包含 2-12 个代码".into());
    }
    let mut symbols = Vec::with_capacity(raw_symbols.len());
    let mut unique = HashSet::new();
    for raw in raw_symbols {
        let symbol = Symbol::new(raw.trim()).map_err(|error| error.to_string())?;
        if !unique.insert(symbol.code().to_string()) {
            return Err(format!("symbols 包含重复证券代码：{}", symbol.code()));
        }
        symbols.push(symbol);
    }
    let window_days = window_days.unwrap_or(250).clamp(60, 500);
    let fetched = stream::iter(symbols)
        .map(|symbol| async move {
            market
                .kline(&symbol, KlinePeriod::Day, Adjust::Qfq, window_days)
                .await
                .map(|value| {
                    (
                        symbol.code().to_string(),
                        value.data,
                        value.source.to_string(),
                    )
                })
                .map_err(|error| format!("{}: {error}", symbol.code()))
        })
        .buffer_unordered(4)
        .collect::<Vec<_>>()
        .await;
    let mut series = Vec::new();
    let mut errors = Vec::new();
    for item in fetched {
        match item {
            Ok(value) => series.push(value),
            Err(error) => errors.push(error),
        }
    }
    series.sort_by(|left, right| left.0.cmp(&right.0));
    if series.len() < 2 {
        return Err(format!("可用 K 线序列不足 2 条：{}", errors.join("；")));
    }
    let mut common = series[0]
        .1
        .iter()
        .map(|bar| bar.date)
        .collect::<HashSet<_>>();
    for (_, bars, _) in &series[1..] {
        let dates = bars.iter().map(|bar| bar.date).collect::<HashSet<_>>();
        common.retain(|date| dates.contains(date));
    }
    if common.len() < REL_MIN_ALIGNED {
        return Err(format!(
            "重叠交易日不足：仅 {} 天，至少需要 {REL_MIN_ALIGNED}",
            common.len()
        ));
    }
    let mut dates = common.into_iter().collect::<Vec<_>>();
    dates.sort_unstable();
    let mut returns = Vec::with_capacity(series.len());
    for (code, bars, _) in &series {
        let by_date = bars
            .iter()
            .map(|bar| (bar.date, bar.close))
            .collect::<HashMap<_, _>>();
        let closes = dates.iter().map(|date| by_date[date]).collect::<Vec<_>>();
        let values = astock_quant::returns::arithmetic_returns(&closes)
            .map_err(|error| format!("{code} 收益率计算失败：{error}"))?;
        returns.push(values);
    }
    let labels = series
        .iter()
        .map(|(code, _, _)| code.clone())
        .collect::<Vec<_>>();
    let sources = series
        .iter()
        .map(|(code, _, source)| json!({"symbol": code, "source": source}))
        .collect::<Vec<_>>();
    let mut matrix = vec![vec![None; labels.len()]; labels.len()];
    for (index, row) in matrix.iter_mut().enumerate() {
        row[index] = Some(1.0);
    }
    let mut edges = Vec::new();
    for left in 0..labels.len() {
        for right in (left + 1)..labels.len() {
            match pair_edge(
                &returns[left],
                &returns[right],
                &labels[left],
                &labels[right],
            ) {
                Ok((edge, correlation)) => {
                    matrix[left][right] = Some(correlation);
                    matrix[right][left] = Some(correlation);
                    edges.push(edge);
                }
                Err(error) => errors.push(format!("{}-{}: {error}", labels[left], labels[right])),
            }
        }
    }
    Ok(json!({
        "window_days": window_days,
        "aligned_bars": dates.len(),
        "period": {
            "start": dates.first().map(ToString::to_string),
            "end": dates.last().map(ToString::to_string),
        },
        "nodes": labels.iter().map(|code| json!({"symbol": code})).collect::<Vec<_>>(),
        "edges": edges,
        "matrix": {"labels": labels, "pearson": matrix},
        "sources": sources,
        "method": "日频前复权收盘算术收益率；共同交易日内连接；Pearson + ±5 日交叉相关 + 199 次固定种子循环块 bootstrap",
        "note": REL_NOTE,
        "errors": errors,
    }))
}

async fn ensure_company(
    graph: &GraphStore,
    market: &MarketData,
    fundamental: &FundamentalClient,
    storage: &Storage,
    raw: &str,
) -> Result<(), String> {
    if graph
        .find_node(raw)
        .await
        .map_err(|error| error.to_string())?
        .is_some()
    {
        return Ok(());
    }
    let symbol = match Symbol::new(raw) {
        Ok(symbol) => symbol,
        Err(_) => return Ok(()),
    };
    if market.security_master.get(symbol.code()).is_none() {
        let _ = market.all_a_shares().await;
    }
    let mut identity = market.security_master.get(symbol.code());
    let profile = fundamental
        .profile(&symbol)
        .await
        .ok()
        .map(|value| value.data);
    let name = identity
        .as_ref()
        .map(|record| record.canonical_name.clone())
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            profile
                .as_ref()
                .map(|value| value.short_name.clone())
                .filter(|value| !value.trim().is_empty())
        })
        .ok_or_else(|| format!("无法从证券主数据或 F10 解析 {} 的身份", symbol.code()))?;
    let company_id = format!("company:{}", symbol.code());
    graph
        .upsert_node(&Node {
            id: company_id.clone(),
            kind: NodeKind::Company,
            name,
            code: Some(symbol.code().into()),
            meta: json!({
                "source": identity.as_ref().map_or("eastmoney_f10", |record| record.source.as_str()),
                "dynamic": true,
                "coverage": "identity",
            }),
        })
        .await
        .map_err(|error| error.to_string())?;
    if let Some(industry) = profile.as_ref().and_then(|value| value.industry.clone()) {
        let industry_id = format!("industry:f10:{industry}");
        graph
            .upsert_node(&Node {
                id: industry_id.clone(),
                kind: NodeKind::Industry,
                name: industry.clone(),
                code: None,
                meta: json!({"source": "eastmoney_f10", "dynamic": true}),
            })
            .await
            .map_err(|error| error.to_string())?;
        graph
            .upsert_edge(&Edge {
                id: None,
                src: company_id,
                dst: industry_id,
                relation: Relation::BelongsTo,
                weight: 1.0,
                source_name: "东方财富 F10 公司概况".into(),
                source_url: format!(
                    "https://emweb.securities.eastmoney.com/PC_HSF10/CompanySurvey/Index?type=web&code={}{}",
                    symbol.market(),
                    symbol.code()
                ),
                confidence: 0.95,
                valid_from: now_secs(),
                valid_to: None,
            })
            .await
            .map_err(|error| error.to_string())?;
        if let Some(mut record) = identity.take() {
            record.industry = Some(industry);
            record.refreshed_at = astock_core::time::utc_now();
            market.security_master.upsert(record.clone());
            storage
                .securities_upsert(vec![record])
                .await
                .map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

fn pair_edge(
    left: &[f64],
    right: &[f64],
    left_code: &str,
    right_code: &str,
) -> Result<(Value, f64), String> {
    let correlation =
        astock_quant::correlation::pearson(left, right).map_err(|error| error.to_string())?;
    let scan = astock_quant::leadlag::cross_correlation_scan(left, right, REL_MAX_LAG)
        .map_err(|error| error.to_string())?;
    let p_value = astock_quant::leadlag::leadlag_bootstrap_pvalue(
        left,
        right,
        scan.best_lag,
        REL_BOOT_BLOCK,
        REL_BOOT_REPS,
        42,
    )
    .ok();
    let leader = match scan.best_lag.cmp(&0) {
        std::cmp::Ordering::Greater => Some(left_code),
        std::cmp::Ordering::Less => Some(right_code),
        std::cmp::Ordering::Equal => None,
    };
    let correlation = round4(correlation);
    Ok((
        json!({
            "pair": [left_code, right_code],
            "pearson": correlation,
            "best_lag": scan.best_lag,
            "lag_corr": round4(scan.best_value),
            "p_value": p_value.map(round4),
            "significant": p_value.is_some_and(|value| value < 0.05),
            "leader": leader,
        }),
        correlation,
    ))
}

fn impact_entry_json(entry: &ImpactEntry) -> Value {
    json!({
        "node_id": entry.node_id,
        "code": entry.code,
        "name": entry.name,
        "direction": entry.direction.label(),
        "hop": entry.hop,
        "logic_chain": entry.logic_chain,
        "expected_lag_days": entry.expected_lag_days,
        "magnitude_estimate_pct": entry.magnitude_estimate.map(|value| round4(value * 100.0)),
        "confidence": round4(entry.confidence),
        "provenance": entry.provenance.iter().map(|(source, url)| json!({"source": source, "url": url})).collect::<Vec<_>>(),
    })
}

fn impact_report_json(report: &ImpactReport) -> Value {
    let entries = |values: &[ImpactEntry]| values.iter().map(impact_entry_json).collect::<Vec<_>>();
    json!({
        "event_title": report.event_title,
        "subject": {
            "id": report.subject.id,
            "name": report.subject.name,
            "kind": report.subject.kind.as_str(),
        },
        "summary": report.summary,
        "primary_benefit": entries(&report.primary_benefit),
        "primary_harm": entries(&report.primary_harm),
        "secondary_benefit": entries(&report.secondary_benefit),
        "secondary_harm": entries(&report.secondary_harm),
        "potential": entries(&report.potential),
        "disclaimer": report.disclaimer,
    })
}

fn validate_text(raw: &str, field: &str, max_len: usize) -> Result<String, String> {
    let value = raw.trim();
    if value.is_empty() {
        return Err(format!("{field} 不能为空"));
    }
    if value.len() > max_len || value.chars().any(char::is_control) {
        return Err(format!("{field} 包含控制字符或超过 {max_len} 字节"));
    }
    Ok(value.into())
}

fn validate_time(value: i64, field: &str) -> Result<(), String> {
    if value <= 0 {
        return Err(format!("{field} 必须是正的 Unix 秒时间戳"));
    }
    Ok(())
}

fn round4(value: f64) -> f64 {
    (value * 10_000.0).round() / 10_000.0
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_secs().min(i64::MAX as u64) as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graph_times_and_relationship_inputs_are_strict() {
        assert!(validate_time(0, "business_time").is_err());
        assert!(validate_text("node\nforged", "node", 256).is_err());
    }

    #[test]
    fn pair_failure_is_not_represented_as_zero_correlation() {
        assert!(pair_edge(&[1.0, 1.0], &[2.0, 2.0], "a", "b").is_err());
    }
}
