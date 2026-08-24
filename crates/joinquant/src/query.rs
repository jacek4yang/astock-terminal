//! Query templates (Python 3.6 compatible) and payload parsing.
//!
//! Facts honored here (docs/data-source-joinquant-v2.md §2.5, §4.5, §4.7):
//!
//! - The research environment's default context date is stuck at
//!   **2015-12-31** — every template passes explicit `start_date`/`end_date`
//!   (or `date=`). The unit tests assert the templates contain them.
//! - Kernel stdout is not UTF-8 — results are printed as one
//!   `JQJSON:<base64(utf-8 json)>` line; the client decodes it.
//! - All values are stringified kernel-side (`astype(str)`) so numpy/pandas
//!   version differences never leak into the JSON; `nan`/`None`/`NaT` map to
//!   `None` here.
//! - Macro tables use the `MAC_` prefix (e.g. `MAC_CPI_MONTH`).

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use regex::Regex;
use serde::Serialize;
use serde_json::Value;

use crate::error::JoinQuantError;

/// Stdout line prefix carrying the base64-wrapped JSON payload.
pub(crate) const OUTPUT_PREFIX: &str = "JQJSON:";

/// One OHLCV daily bar (values may be `None` for suspended sessions).
#[derive(Debug, Clone, PartialEq)]
pub struct DailyBar {
    /// Trading date, `YYYY-MM-DD`.
    pub date: String,
    /// Open price (前复权).
    pub open: Option<f64>,
    /// High price.
    pub high: Option<f64>,
    /// Low price.
    pub low: Option<f64>,
    /// Close price.
    pub close: Option<f64>,
    /// Volume (股).
    pub volume: Option<f64>,
    /// Turnover (元).
    pub money: Option<f64>,
}

/// Valuation snapshot for one security on one date.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ValuationSnapshot {
    /// Internal security code (`SH600000` / `SZ000001` style).
    pub code: String,
    /// 市盈率 (PE-TTM).
    pub pe_ratio: Option<f64>,
    /// 市净率.
    pub pb_ratio: Option<f64>,
    /// 市销率 (PS-TTM).
    pub ps_ratio: Option<f64>,
    /// 市现率 (PCF-TTM).
    pub pcf_ratio: Option<f64>,
    /// 总市值 (亿元).
    pub market_cap: Option<f64>,
    /// 流通市值 (亿元).
    pub circulating_market_cap: Option<f64>,
}

/// Map a JoinQuant code to the terminal's internal code:
/// `000300.XSHG` → `SH000300`, `000001.XSHE` → `SZ000001`.
/// Codes without a known suffix pass through unchanged.
pub fn jq_to_internal(code: &str) -> String {
    match code.split_once('.') {
        Some((num, "XSHG")) => format!("SH{num}"),
        Some((num, "XSHE")) => format!("SZ{num}"),
        _ => code.to_string(),
    }
}

fn validate_security(code: &str) -> Result<(), JoinQuantError> {
    let re = Regex::new(r"^[0-9A-Za-z]+\.(XSHG|XSHE)$").expect("security regex");
    if re.is_match(code) {
        Ok(())
    } else {
        Err(JoinQuantError::InvalidInput(format!(
            "bad security code: {code:?} (expected e.g. 000300.XSHG)"
        )))
    }
}

fn validate_date(date: &str) -> Result<(), JoinQuantError> {
    let re = Regex::new(r"^\d{4}-\d{2}-\d{2}$").expect("date regex");
    if re.is_match(date) {
        Ok(())
    } else {
        Err(JoinQuantError::InvalidInput(format!(
            "bad date: {date:?} (expected YYYY-MM-DD)"
        )))
    }
}

/// Emit helper: every template ends with this base64-wrapped print.
fn emit(payload_expr: &str) -> String {
    format!(
        "print('{OUTPUT_PREFIX}' + _b64.b64encode(\
         _json.dumps({payload_expr}, ensure_ascii=False).encode('utf-8')).decode('ascii'))"
    )
}

/// Python template: daily bars with **explicit dates** (2015 trap guard).
pub(crate) fn daily_code(security: &str, start: &str, end: &str) -> Result<String, JoinQuantError> {
    validate_security(security)?;
    validate_date(start)?;
    validate_date(end)?;
    Ok(format!(
        "import json as _json, base64 as _b64\n\
         _d = get_price('{security}', start_date='{start}', end_date='{end}', \
         frequency='daily', fq='pre', \
         fields=['open','high','low','close','volume','money'])\n\
         _d = _d.reset_index()\n\
         {}",
        emit("_d.astype(str).to_dict('records')")
    ))
}

/// Python template: index components on an explicit date.
pub(crate) fn index_components_code(index: &str, date: &str) -> Result<String, JoinQuantError> {
    validate_security(index)?;
    validate_date(date)?;
    Ok(format!(
        "import json as _json, base64 as _b64\n\
         _l = get_index_stocks('{index}', date='{date}')\n\
         {}",
        emit("list(_l)")
    ))
}

/// Python template: valuation snapshot for a set of securities on a date.
pub(crate) fn valuation_code(codes: &[String], date: &str) -> Result<String, JoinQuantError> {
    if codes.is_empty() {
        return Err(JoinQuantError::InvalidInput("empty code list".into()));
    }
    for c in codes {
        validate_security(c)?;
    }
    validate_date(date)?;
    let list = codes
        .iter()
        .map(|c| format!("'{c}'"))
        .collect::<Vec<_>>()
        .join(",");
    Ok(format!(
        "import json as _json, base64 as _b64\n\
         _q = query(valuation.code, valuation.pe_ratio, valuation.pb_ratio, \
         valuation.ps_ratio, valuation.pcf_ratio, valuation.market_cap, \
         valuation.circulating_market_cap).filter(valuation.code.in_([{list}]))\n\
         _d = get_fundamentals(_q, date='{date}')\n\
         _d = _d.reset_index()\n\
         {}",
        emit("_d.astype(str).to_dict('records')")
    ))
}

/// Python template: latest `limit` monthly CPI rows from the macro library
/// (`MAC_` table prefix — doc §2.5).
pub(crate) fn macro_cpi_code(limit: usize) -> String {
    format!(
        "import json as _json, base64 as _b64\n\
         from jqdata import macro\n\
         _d = macro.run_query(query(macro.MAC_CPI_MONTH)\
         .order_by(macro.MAC_CPI_MONTH.stat_month.desc()).limit({limit}))\n\
         _d = _d.reset_index()\n\
         {}",
        emit("_d.astype(str).to_dict('records')")
    )
}

/// Extract the base64-wrapped JSON payload from aggregated kernel stdout.
pub(crate) fn extract_payload(stdout: &str) -> Result<Value, JoinQuantError> {
    let line = stdout
        .lines()
        .rev()
        .find(|l| l.starts_with(OUTPUT_PREFIX))
        .ok_or(JoinQuantError::OutputMissing)?;
    let bytes = B64.decode(line[OUTPUT_PREFIX.len()..].trim())?;
    Ok(serde_json::from_slice(&bytes)?)
}

/// Parse a stringified numeric cell; `nan`/`None`/`NaT`/empty → `None`.
pub(crate) fn parse_opt_f64(s: &str) -> Option<f64> {
    match s.trim() {
        "" | "nan" | "NaN" | "None" | "NaT" => None,
        other => other.parse().ok(),
    }
}

fn cell<'a>(rec: &'a Value, key: &str) -> Option<&'a str> {
    rec.get(key).and_then(Value::as_str)
}

/// Parse `daily` records (reset_index → date column is named `index`).
pub(crate) fn parse_daily_bars(payload: &Value) -> Vec<DailyBar> {
    payload
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|rec| {
            let raw_date = cell(rec, "index").or_else(|| cell(rec, "time"))?;
            let date = raw_date.get(..10).unwrap_or(raw_date).to_string();
            Some(DailyBar {
                date,
                open: cell(rec, "open").and_then(parse_opt_f64),
                high: cell(rec, "high").and_then(parse_opt_f64),
                low: cell(rec, "low").and_then(parse_opt_f64),
                close: cell(rec, "close").and_then(parse_opt_f64),
                volume: cell(rec, "volume").and_then(parse_opt_f64),
                money: cell(rec, "money").and_then(parse_opt_f64),
            })
        })
        .collect()
}

/// Parse the index-components payload (JSON array of JQ codes) into
/// internal codes.
pub(crate) fn parse_components(payload: &Value) -> Vec<String> {
    payload
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(jq_to_internal)
        .collect()
}

/// Parse valuation records.
pub(crate) fn parse_valuations(payload: &Value) -> Vec<ValuationSnapshot> {
    payload
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|rec| {
            let code = jq_to_internal(cell(rec, "code")?);
            Some(ValuationSnapshot {
                code,
                pe_ratio: cell(rec, "pe_ratio").and_then(parse_opt_f64),
                pb_ratio: cell(rec, "pb_ratio").and_then(parse_opt_f64),
                ps_ratio: cell(rec, "ps_ratio").and_then(parse_opt_f64),
                pcf_ratio: cell(rec, "pcf_ratio").and_then(parse_opt_f64),
                market_cap: cell(rec, "market_cap").and_then(parse_opt_f64),
                circulating_market_cap: cell(rec, "circulating_market_cap").and_then(parse_opt_f64),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn wrap_in_stdout(v: &Value) -> String {
        let b64 = B64.encode(serde_json::to_vec(v).unwrap());
        format!("some noisy kernel output\n{OUTPUT_PREFIX}{b64}\ntrailing\n")
    }

    #[test]
    fn jq_code_mapping() {
        assert_eq!(jq_to_internal("000300.XSHG"), "SH000300");
        assert_eq!(jq_to_internal("000001.XSHE"), "SZ000001");
        assert_eq!(jq_to_internal("SH000300"), "SH000300");
        assert_eq!(jq_to_internal("830799.XBJJ"), "830799.XBJJ");
    }

    #[test]
    fn daily_template_has_explicit_dates() {
        // Guard against the 2015-12-31 default-context trap (doc §2.5).
        let code = daily_code("000300.XSHG", "2026-08-01", "2026-08-21").unwrap();
        assert!(code.contains("start_date='2026-08-01'"));
        assert!(code.contains("end_date='2026-08-21'"));
        assert!(code.contains("frequency='daily'"));
        assert!(code.contains(OUTPUT_PREFIX));
        assert!(daily_code("not a code; import os", "2026-08-01", "2026-08-21").is_err());
        assert!(daily_code("000300.XSHG", "2026/08/01", "2026-08-21").is_err());
    }

    #[test]
    fn index_template_has_explicit_date() {
        let code = index_components_code("000300.XSHG", "2026-08-21").unwrap();
        assert!(code.contains("get_index_stocks('000300.XSHG', date='2026-08-21')"));
    }

    #[test]
    fn valuation_template_has_date_and_codes() {
        let codes = vec!["000001.XSHE".to_string(), "600000.XSHG".to_string()];
        let code = valuation_code(&codes, "2026-08-20").unwrap();
        assert!(code.contains("date='2026-08-20'"));
        assert!(code.contains("'000001.XSHE','600000.XSHG'"));
        assert!(valuation_code(&[], "2026-08-20").is_err());
    }

    #[test]
    fn macro_template_uses_mac_prefix() {
        let code = macro_cpi_code(24);
        assert!(code.contains("macro.MAC_CPI_MONTH"));
        assert!(code.contains("limit(24)"));
    }

    #[test]
    fn extract_payload_finds_last_prefixed_line() {
        let payload = json!([{"a": "1"}]);
        let stdout = wrap_in_stdout(&payload);
        assert_eq!(extract_payload(&stdout).unwrap(), payload);
        assert!(matches!(
            extract_payload("no payload here"),
            Err(JoinQuantError::OutputMissing)
        ));
    }

    #[test]
    fn extract_payload_handles_chinese_via_base64() {
        let payload = json!([{"area_name": "全国"}]);
        let stdout = wrap_in_stdout(&payload);
        let v = extract_payload(&stdout).unwrap();
        assert_eq!(v[0]["area_name"], "全国");
    }

    #[test]
    fn parse_opt_f64_maps_missing_values() {
        assert_eq!(parse_opt_f64("5.09"), Some(5.09));
        assert_eq!(parse_opt_f64("nan"), None);
        assert_eq!(parse_opt_f64("None"), None);
        assert_eq!(parse_opt_f64("NaT"), None);
        assert_eq!(parse_opt_f64(""), None);
        assert_eq!(parse_opt_f64("abc"), None);
    }

    #[test]
    fn parse_daily_bars_from_records() {
        let payload = json!([
            {"index": "2026-08-20 00:00:00", "open": "10.1", "high": "10.5",
             "low": "10.0", "close": "10.4", "volume": "123456", "money": "1283534.4"},
            {"index": "2026-08-21 00:00:00", "open": "nan", "high": "nan",
             "low": "nan", "close": "nan", "volume": "0", "money": "0"}
        ]);
        let bars = parse_daily_bars(&payload);
        assert_eq!(bars.len(), 2);
        assert_eq!(bars[0].date, "2026-08-20");
        assert_eq!(bars[0].close, Some(10.4));
        assert_eq!(bars[1].date, "2026-08-21");
        assert_eq!(bars[1].close, None);
    }

    #[test]
    fn parse_components_maps_to_internal_codes() {
        let payload = json!(["000001.XSHE", "600000.XSHG"]);
        assert_eq!(parse_components(&payload), vec!["SZ000001", "SH600000"]);
    }

    #[test]
    fn parse_valuations_from_records() {
        let payload = json!([{
            "code": "000001.XSHE", "pe_ratio": "5.09", "pb_ratio": "0.42",
            "ps_ratio": "1.1", "pcf_ratio": "nan",
            "market_cap": "2212.5", "circulating_market_cap": "2210.1"
        }]);
        let v = parse_valuations(&payload);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].code, "SZ000001");
        assert_eq!(v[0].pe_ratio, Some(5.09));
        assert_eq!(v[0].pcf_ratio, None);
    }
}
