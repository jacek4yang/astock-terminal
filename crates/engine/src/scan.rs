//! Cancellable full-market research scan owned by the Engine process.

use std::sync::Arc;

use astock_core::{Adjust, Bar, KlinePeriod, MarketBreadth, StockListItem, Symbol};
use astock_market_data::{DataProvider, MarketData};
use astock_trading_rules::RuleSet;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::analysis;

const SCAN_CONCURRENCY: usize = 15;
const SCAN_TOP_N: usize = 50;
const MIN_BARS: usize = 30;
const SCAN_KLINE_COUNT: u32 = 250;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScanHit {
    pub symbol: String,
    pub name: String,
    pub score: f64,
    pub action: String,
    pub confidence: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanSnapshot {
    pub job_id: Option<String>,
    pub running: bool,
    pub cancelling: bool,
    pub done: u32,
    pub total: u32,
    pub current_symbol: String,
    pub skipped: u32,
    pub results: Vec<ScanHit>,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
    pub source_version_id: Option<String>,
    pub failure: Option<String>,
    pub limitation: Option<String>,
}

impl Default for ScanSnapshot {
    fn default() -> Self {
        Self {
            job_id: None,
            running: false,
            cancelling: false,
            done: 0,
            total: 0,
            current_symbol: String::new(),
            skipped: 0,
            results: Vec::new(),
            started_at: None,
            finished_at: None,
            source_version_id: None,
            failure: None,
            limitation: Some("扫描只生成研究候选，不会连接券商、自动下单或后台执行交易".into()),
        }
    }
}

struct ScanState {
    generation: u64,
    snapshot: ScanSnapshot,
    cancel: Option<CancellationToken>,
}

#[derive(Clone)]
pub struct ScanService {
    inner: Arc<Mutex<ScanState>>,
}

impl Default for ScanService {
    fn default() -> Self {
        Self {
            inner: Arc::new(Mutex::new(ScanState {
                generation: 0,
                snapshot: ScanSnapshot::default(),
                cancel: None,
            })),
        }
    }
}

impl ScanService {
    pub async fn start(
        &self,
        market: Arc<MarketData>,
        rules: RuleSet,
    ) -> Result<ScanSnapshot, &'static str> {
        let (generation, token, snapshot) = {
            let mut state = self.inner.lock().await;
            if state.snapshot.running {
                return Err("a market scan is already running; cancel it before starting another");
            }
            state.generation = state.generation.saturating_add(1);
            let generation = state.generation;
            let token = CancellationToken::new();
            let now = chrono::Utc::now();
            state.snapshot = ScanSnapshot {
                job_id: Some(format!("scan-{}-{generation}", now.timestamp_millis())),
                running: true,
                started_at: Some(now.timestamp()),
                ..ScanSnapshot::default()
            };
            state.cancel = Some(token.clone());
            (generation, token, state.snapshot.clone())
        };
        let service = self.clone();
        tokio::spawn(async move {
            service.run(generation, market, rules, token).await;
        });
        Ok(snapshot)
    }

    pub async fn status(&self) -> ScanSnapshot {
        self.inner.lock().await.snapshot.clone()
    }

    pub async fn cancel(&self) -> bool {
        let mut state = self.inner.lock().await;
        let Some(token) = state.cancel.as_ref().cloned() else {
            return false;
        };
        state.snapshot.cancelling = true;
        token.cancel();
        true
    }

    async fn run(
        &self,
        generation: u64,
        market: Arc<MarketData>,
        rules: RuleSet,
        token: CancellationToken,
    ) {
        let fetched = match market.all_a_shares().await {
            Ok(value) => value,
            Err(error) => {
                self.finish(generation, Some(format!("全市场证券列表获取失败：{error}")))
                    .await;
                return;
            }
        };
        let source_version_id = format!(
            "market-scan:{}:{}",
            fetched.source,
            fetched.fetched_at.timestamp_millis()
        );
        let candidates = scan_prefilter(&fetched.data);
        {
            let mut state = self.inner.lock().await;
            if state.generation != generation {
                return;
            }
            state.snapshot.total = candidates.len() as u32;
            state.snapshot.source_version_id = Some(source_version_id);
        }

        let (index, breadth) =
            tokio::join!(market.index_kline("1.000001", 60), market.market_breadth());
        let index = index.ok().map(|value| value.data).map(Arc::new);
        let breadth = breadth.ok().map(|value| value.data);
        let stream = futures::stream::iter(candidates).map(|stock| {
            let market = market.clone();
            let rules = rules.clone();
            let index = index.clone();
            async move { scan_one(market, &rules, stock, index.as_deref(), breadth.as_ref()).await }
        });
        let stream = stream.buffer_unordered(SCAN_CONCURRENCY);
        tokio::pin!(stream);

        loop {
            let next = tokio::select! {
                _ = token.cancelled() => break,
                item = stream.next() => item,
            };
            let Some((symbol, hit)) = next else { break };
            let mut state = self.inner.lock().await;
            if state.generation != generation {
                return;
            }
            state.snapshot.done = state.snapshot.done.saturating_add(1);
            state.snapshot.current_symbol = symbol;
            if let Some(hit) = hit {
                state.snapshot.results.push(hit);
            } else {
                state.snapshot.skipped = state.snapshot.skipped.saturating_add(1);
            }
        }
        self.finish(generation, None).await;
    }

    async fn finish(&self, generation: u64, failure: Option<String>) {
        let mut state = self.inner.lock().await;
        if state.generation != generation {
            return;
        }
        state
            .snapshot
            .results
            .sort_by(|left, right| right.score.total_cmp(&left.score));
        state.snapshot.results.truncate(SCAN_TOP_N);
        state.snapshot.running = false;
        state.snapshot.cancelling = false;
        state.snapshot.finished_at = Some(chrono::Utc::now().timestamp());
        state.snapshot.failure = failure;
        state.cancel = None;
    }
}

async fn scan_one(
    market: Arc<MarketData>,
    rules: &RuleSet,
    stock: StockListItem,
    index: Option<&Vec<Bar>>,
    breadth: Option<&MarketBreadth>,
) -> (String, Option<ScanHit>) {
    let code = stock.code.clone();
    let Ok(symbol) = Symbol::new(&code) else {
        return (code, None);
    };
    let (bars, quote, flows) = tokio::join!(
        market.scan_kline(&symbol, KlinePeriod::Day, Adjust::Qfq, SCAN_KLINE_COUNT),
        market.quote(&symbol),
        market.fund_flow_daily(&symbol, 30),
    );
    let (Ok(bars), Ok(quote)) = (bars, quote) else {
        return (code, None);
    };
    if bars.data.len() < MIN_BARS {
        return (code, None);
    }
    let signal = analysis::signal_with_context(
        rules,
        &symbol,
        &bars.data,
        &quote.data,
        flows.as_ref().ok().map(|value| value.data.as_slice()),
        index.map(Vec::as_slice),
        breadth,
        &bars.source.to_string(),
    );
    let hit = hit_from_signal(
        &code,
        (!quote.data.name.is_empty())
            .then(|| quote.data.name.clone())
            .unwrap_or(stock.name),
        &signal,
    );
    (code, hit)
}

fn scan_prefilter(items: &[StockListItem]) -> Vec<StockListItem> {
    items
        .iter()
        .filter(|item| {
            !item.name.contains("ST")
                && !item.name.contains('退')
                && item.price.is_some_and(|price| price > 0.0)
        })
        .cloned()
        .collect()
}

fn hit_from_signal(symbol: &str, name: String, signal: &Value) -> Option<ScanHit> {
    let action = signal.get("action").and_then(Value::as_str)?;
    if !matches!(action, "强烈买入" | "买入" | "谨慎买入") {
        return None;
    }
    Some(ScanHit {
        symbol: symbol.into(),
        name,
        score: signal.get("score").and_then(Value::as_f64).unwrap_or(0.0),
        action: action.into(),
        confidence: signal
            .get("confidence")
            .and_then(Value::as_f64)
            .unwrap_or(0.0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(code: &str, name: &str, price: f64) -> StockListItem {
        StockListItem {
            code: code.into(),
            name: name.into(),
            price: Some(price),
            pct: Some(0.0),
            amount: Some(0.0),
        }
    }

    #[test]
    fn prefilter_keeps_normal_securities_and_drops_risk_names_or_suspension() {
        let rows = [
            item("600519", "贵州茅台", 1500.0),
            item("600001", "ST样本", 2.0),
            item("600002", "退市样本", 1.0),
            item("600003", "停牌样本", 0.0),
        ];
        let kept = scan_prefilter(&rows);
        assert_eq!(
            kept.iter().map(|row| row.code.as_str()).collect::<Vec<_>>(),
            ["600519"]
        );
    }

    #[test]
    fn only_buy_actions_become_candidates() {
        assert!(hit_from_signal(
            "600519",
            "贵州茅台".into(),
            &serde_json::json!({"action":"买入","score":70.0,"confidence":80.0}),
        )
        .is_some());
        assert!(hit_from_signal(
            "600519",
            "贵州茅台".into(),
            &serde_json::json!({"action":"观望","score":70.0,"confidence":80.0}),
        )
        .is_none());
    }
}
