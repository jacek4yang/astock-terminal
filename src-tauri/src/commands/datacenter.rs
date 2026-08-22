//! EastMoney datacenter commands: limit-up pool, 龙虎榜, 两融, 机构调研,
//! 股东户数, 业绩预告, 限售解禁, 停复牌, 公告与板块/成分股 — the
//! `state.market.em_datacenter` adapter does the fetching; this module is a
//! thin validation + 60s in-memory cache layer (same pattern as
//! `cache_path::INTRADAY_THROTTLE`; the adapter itself additionally caches
//! reports for 600s, so the command layer mainly absorbs rapid UI polling).
//!
//! All commands return `{rows, count, source, fetched_at}`; row shapes are
//! the `astock_market_data::providers::em_datacenter` row structs serialized
//! as-is (dates as `YYYY-MM-DD`, amounts/元, ratios/%, 亿元字段以 `_yi`
//! 结尾), documented per command in `docs/command-contract.md`.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use astock_core::{Fetched, Symbol};
use astock_market_data::providers::{BoardKind, NoticeNode};
use astock_trading_rules::RuleSet;
use chrono::{Datelike, NaiveDate};
use serde::Serialize;
use serde_json::{json, Value};
use tauri::State;

use crate::cache_path::{latest_trading_day_on_or_before, shanghai_now};
use crate::error::CmdError;
use crate::state::AppState;

/// Command-layer in-memory cache TTL (60s; see module docs).
const DC_CACHE_TTL: Duration = Duration::from_secs(60);

/// `(cached_at, payload)` per command+params key.
static DC_CACHE: LazyLock<Mutex<HashMap<String, (Instant, Value)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Fetch a still-valid cached payload at `now` (miss past [`DC_CACHE_TTL`]).
fn cache_get(key: &str, now: Instant) -> Option<Value> {
    let map = DC_CACHE.lock().expect("datacenter cache poisoned");
    let (cached_at, value) = map.get(key)?;
    (now.duration_since(*cached_at) < DC_CACHE_TTL).then(|| value.clone())
}

/// Store a payload at `now`, replacing any previous entry for the key.
fn cache_put(key: String, value: Value, now: Instant) {
    DC_CACHE
        .lock()
        .expect("datacenter cache poisoned")
        .insert(key, (now, value));
}

/// Serve `key` from the 60s in-memory cache, or run `fetch` and cache the
/// result. Errors are never cached.
async fn cached_json(
    key: String,
    fetch: impl std::future::Future<Output = Result<Value, CmdError>>,
) -> Result<Value, CmdError> {
    if let Some(hit) = cache_get(&key, Instant::now()) {
        return Ok(hit);
    }
    let value = fetch.await?;
    cache_put(key, value.clone(), Instant::now());
    Ok(value)
}

/// Uniform payload: rows + count + upstream provenance.
fn rows_payload<T: Serialize>(fetched: Fetched<Vec<T>>) -> Result<Value, CmdError> {
    let rows = serde_json::to_value(fetched.data)
        .map_err(|e| CmdError::new("engine", format!("serialize datacenter rows: {e}")))?;
    let count = rows.as_array().map_or(0, Vec::len);
    Ok(json!({
        "rows": rows,
        "count": count,
        "source": fetched.source.to_string(),
        "fetched_at": fetched.fetched_at.to_rfc3339(),
    }))
}

/// Parse a `YYYY-MM-DD` date argument.
fn parse_date(raw: &str, arg: &str) -> Result<NaiveDate, CmdError> {
    NaiveDate::parse_from_str(raw.trim(), "%Y-%m-%d").map_err(|_| {
        CmdError::new(
            "invalid_param",
            format!("{arg} 必须是 YYYY-MM-DD 日期,收到 `{raw}`"),
        )
    })
}

/// Latest trading day on or before today (Asia/Shanghai).
fn latest_trading_day(rules: &RuleSet) -> NaiveDate {
    latest_trading_day_on_or_before(rules, shanghai_now().date_naive())
}

/// Most recent quarter end (03-31 / 06-30 / 09-30 / 12-31) on or before
/// `today` — the default `report_date` for 业绩预告.
fn latest_quarter_end(today: NaiveDate) -> NaiveDate {
    let year = today.year();
    let candidates = [
        NaiveDate::from_ymd_opt(year, 3, 31),
        NaiveDate::from_ymd_opt(year, 6, 30),
        NaiveDate::from_ymd_opt(year, 9, 30),
        NaiveDate::from_ymd_opt(year, 12, 31),
        NaiveDate::from_ymd_opt(year - 1, 12, 31),
    ];
    candidates
        .into_iter()
        .flatten()
        .filter(|d| *d <= today)
        .max()
        .expect("previous year's 12-31 is always <= today")
}

/// Parse a board kind: industry / concept (中英文别名均可).
fn parse_board_kind(raw: &str) -> Result<BoardKind, CmdError> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "industry" | "hy" | "行业" => Ok(BoardKind::Industry),
        "concept" | "gn" | "概念" => Ok(BoardKind::Concept),
        other => Err(CmdError::new(
            "invalid_param",
            format!("kind 只能是 industry/concept,收到 `{other}`"),
        )),
    }
}

/// Validate a board code (`BK` + 4 digits, case-insensitive input).
fn parse_board_code(raw: &str) -> Result<String, CmdError> {
    let code = raw.trim().to_ascii_uppercase();
    let ok = code.len() == 6
        && code.starts_with("BK")
        && code[2..].bytes().all(|b| b.is_ascii_digit());
    if ok {
        Ok(code)
    } else {
        Err(CmdError::new(
            "invalid_param",
            format!("bk_code 必须是 BK + 4 位数字(如 BK0447),收到 `{raw}`"),
        ))
    }
}

/// 涨停股池(默认最近交易日,`date` 可指定任一交易日)。
#[tauri::command(rename_all = "snake_case")]
pub async fn get_zt_pool(
    state: State<'_, AppState>,
    date: Option<String>,
) -> Result<Value, CmdError> {
    let date = match &date {
        Some(raw) => parse_date(raw, "date")?,
        None => latest_trading_day(&state.rules),
    };
    let dc = state.market.em_datacenter.clone();
    cached_json(format!("zt_pool|{date}"), async move {
        rows_payload(dc.zt_pool(date).await?)
    })
    .await
}

/// 情绪池统一入口:`pool` = zt 涨停 / prev 昨日涨停 / strong 强势 /
/// sub_new 次新 / broken 炸板 / dt 跌停(默认最近交易日)。
#[tauri::command(rename_all = "snake_case")]
pub async fn get_pool(
    state: State<'_, AppState>,
    pool: String,
    date: Option<String>,
) -> Result<Value, CmdError> {
    let date = match &date {
        Some(raw) => parse_date(raw, "date")?,
        None => latest_trading_day(&state.rules),
    };
    let kind = pool.trim().to_ascii_lowercase();
    let dc = state.market.em_datacenter.clone();
    cached_json(format!("pool|{kind}|{date}"), async move {
        match kind.as_str() {
            "zt" => rows_payload(dc.zt_pool(date).await?),
            "prev" => rows_payload(dc.prev_zt_pool(date).await?),
            "strong" => rows_payload(dc.strong_pool(date).await?),
            "sub_new" => rows_payload(dc.sub_new_pool(date).await?),
            "broken" => rows_payload(dc.broken_pool(date).await?),
            "dt" => rows_payload(dc.dt_pool(date).await?),
            other => Err(CmdError::new(
                "invalid_param",
                format!("pool 只能是 zt/prev/strong/sub_new/broken/dt,收到 `{other}`"),
            )),
        }
    })
    .await
}

/// 龙虎榜详情(近 `days` 个自然日,默认 7;按最近交易日向前取窗)。
#[tauri::command(rename_all = "snake_case")]
pub async fn get_billboard(
    state: State<'_, AppState>,
    days: Option<u32>,
) -> Result<Value, CmdError> {
    let days = days.unwrap_or(7).clamp(1, 90);
    let end = latest_trading_day(&state.rules);
    let start = end - chrono::Duration::days(i64::from(days) - 1);
    // 约 80 行/交易日,页 500 行。
    let max_pages = (days * 100 / 500 + 1).clamp(1, 20);
    let dc = state.market.em_datacenter.clone();
    cached_json(format!("billboard|{start}|{end}"), async move {
        rows_payload(dc.billboard_detail(start, end, max_pages).await?)
    })
    .await
}

/// 两融账户统计(最近约 1000 个交易日,按日期倒序;金额单位亿元,
/// 字段以 `_yi` 结尾)。
#[tauri::command(rename_all = "snake_case")]
pub async fn get_margin_daily(state: State<'_, AppState>) -> Result<Value, CmdError> {
    let dc = state.market.em_datacenter.clone();
    cached_json("margin_daily".to_string(), async move {
        rows_payload(dc.margin_daily(2).await?)
    })
    .await
}

/// 机构调研统计(近 `days` 个自然日,默认 30)。
#[tauri::command(rename_all = "snake_case")]
pub async fn get_org_survey(
    state: State<'_, AppState>,
    days: Option<u32>,
) -> Result<Value, CmdError> {
    let days = days.unwrap_or(30).clamp(1, 365);
    let since = shanghai_now().date_naive() - chrono::Duration::days(i64::from(days));
    let dc = state.market.em_datacenter.clone();
    cached_json(format!("org_survey|{since}"), async move {
        rows_payload(dc.org_survey(since, 4).await?)
    })
    .await
}

/// 股东户数(最新披露;`code` 可选,传入时只返回该股票,未披露则为空)。
#[tauri::command(rename_all = "snake_case")]
pub async fn get_holder_num(
    state: State<'_, AppState>,
    code: Option<String>,
) -> Result<Value, CmdError> {
    let code = code
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(Symbol::new)
        .transpose()?
        .map(|s| s.code().to_string());
    let dc = state.market.em_datacenter.clone();
    cached_json(
        format!("holder_num|{}", code.as_deref().unwrap_or("*")),
        async move {
            // 12 页 × 500 行覆盖全部已披露 A 股。
            let mut fetched = dc.holder_num_latest(12).await?;
            if let Some(code) = &code {
                fetched.data.retain(|r| &r.code == code);
            }
            rows_payload(fetched)
        },
    )
    .await
}

/// 业绩预告(`report_date` 为报告期,默认今天之前最近的季度末,
/// 如 2026-06-30)。
#[tauri::command(rename_all = "snake_case")]
pub async fn get_earnings_predict(
    state: State<'_, AppState>,
    report_date: Option<String>,
) -> Result<Value, CmdError> {
    let report_date = match &report_date {
        Some(raw) => parse_date(raw, "report_date")?,
        None => latest_quarter_end(shanghai_now().date_naive()),
    };
    let dc = state.market.em_datacenter.clone();
    cached_json(format!("earnings_predict|{report_date}"), async move {
        rows_payload(dc.earnings_predict(report_date, 6).await?)
    })
    .await
}

/// 限售解禁明细(解禁窗口 `[start, end]`,最长 366 天)。
#[tauri::command(rename_all = "snake_case")]
pub async fn get_lift_stage(
    state: State<'_, AppState>,
    start: String,
    end: String,
) -> Result<Value, CmdError> {
    let start = parse_date(&start, "start")?;
    let end = parse_date(&end, "end")?;
    if start > end {
        return Err(CmdError::new("invalid_param", "start 不能晚于 end"));
    }
    if (end - start).num_days() > 366 {
        return Err(CmdError::new(
            "invalid_param",
            "解禁窗口最长 366 天,请分段查询",
        ));
    }
    let dc = state.market.em_datacenter.clone();
    cached_json(format!("lift_stage|{start}|{end}"), async move {
        rows_payload(dc.lift_stage(start, end, 4).await?)
    })
    .await
}

/// 停复牌(默认最近交易日;当日无停复牌时 rows 为空,属正常)。
#[tauri::command(rename_all = "snake_case")]
pub async fn get_suspensions(
    state: State<'_, AppState>,
    date: Option<String>,
) -> Result<Value, CmdError> {
    let date = match &date {
        Some(raw) => parse_date(raw, "date")?,
        None => latest_trading_day(&state.rules),
    };
    let dc = state.market.em_datacenter.clone();
    cached_json(format!("suspensions|{date}"), async move {
        rows_payload(dc.suspensions(date, 2).await?)
    })
    .await
}

/// 个股公告(近 `days` 个自然日,默认 90,最多返回最近 300 条)。
#[tauri::command(rename_all = "snake_case")]
pub async fn get_notices(
    state: State<'_, AppState>,
    code: String,
    days: Option<u32>,
) -> Result<Value, CmdError> {
    let symbol = Symbol::new(&code)?;
    let code = symbol.code().to_string();
    let days = days.unwrap_or(90).clamp(1, 730);
    let today = shanghai_now().date_naive();
    let begin = today - chrono::Duration::days(i64::from(days));
    let dc = state.market.em_datacenter.clone();
    cached_json(format!("notices|{code}|{days}"), async move {
        rows_payload(
            dc.notices(Some(&code), NoticeNode::All, Some(begin), Some(today), 3)
                .await?,
        )
    })
    .await
}

/// 板块列表(`kind`: industry 行业 / concept 概念)。
#[tauri::command(rename_all = "snake_case")]
pub async fn get_boards(state: State<'_, AppState>, kind: String) -> Result<Value, CmdError> {
    let kind = parse_board_kind(&kind)?;
    let dc = state.market.em_datacenter.clone();
    cached_json(format!("boards|{kind:?}"), async move {
        rows_payload(dc.board_list(kind).await?)
    })
    .await
}

/// 板块成分股(`bk_code` 为 get_boards 返回的板块代码,如 BK0447)。
#[tauri::command(rename_all = "snake_case")]
pub async fn get_board_cons(
    state: State<'_, AppState>,
    bk_code: String,
) -> Result<Value, CmdError> {
    let bk_code = parse_board_code(&bk_code)?;
    let dc = state.market.em_datacenter.clone();
    cached_json(format!("board_cons|{bk_code}"), async move {
        rows_payload(dc.board_cons(&bk_code, 5).await?)
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_hit_within_ttl_and_miss_after() {
        let key = "test|cache-hit";
        let t0 = Instant::now();
        assert!(cache_get(key, t0).is_none());
        cache_put(key.to_string(), json!({"rows": []}), t0);
        assert!(cache_get(key, t0 + Duration::from_secs(30)).is_some());
        assert!(cache_get(key, t0 + Duration::from_secs(59)).is_some());
        assert!(cache_get(key, t0 + DC_CACHE_TTL).is_none());
    }

    #[test]
    fn parse_date_strict() {
        assert_eq!(
            parse_date("2026-08-22", "date").unwrap(),
            NaiveDate::from_ymd_opt(2026, 8, 22).unwrap()
        );
        assert!(parse_date(" 2026-08-22 ", "date").is_ok(), "trims");
        assert!(parse_date("20260822", "date").is_err());
        assert!(parse_date("", "date").is_err());
    }

    #[test]
    fn latest_quarter_end_picks_previous_quarter() {
        let d = |y, m, dd| NaiveDate::from_ymd_opt(y, m, dd).unwrap();
        // 季度末当天也算。
        assert_eq!(latest_quarter_end(d(2026, 6, 30)), d(2026, 6, 30));
        assert_eq!(latest_quarter_end(d(2026, 8, 22)), d(2026, 6, 30));
        assert_eq!(latest_quarter_end(d(2026, 1, 15)), d(2025, 12, 31));
        assert_eq!(latest_quarter_end(d(2026, 4, 1)), d(2026, 3, 31));
    }

    #[test]
    fn board_kind_accepts_aliases() {
        assert_eq!(parse_board_kind("industry").unwrap(), BoardKind::Industry);
        assert_eq!(parse_board_kind("行业").unwrap(), BoardKind::Industry);
        assert_eq!(parse_board_kind("CONCEPT").unwrap(), BoardKind::Concept);
        assert!(parse_board_kind("region").is_err());
    }

    #[test]
    fn board_code_validated() {
        assert_eq!(parse_board_code("bk0447").unwrap(), "BK0447");
        assert!(parse_board_code("BK044").is_err());
        assert!(parse_board_code("600519").is_err());
        assert!(parse_board_code("BK04A7").is_err());
    }
}
