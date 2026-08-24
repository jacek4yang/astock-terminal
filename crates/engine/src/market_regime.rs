//! Evidence-bearing market regime classification.

use astock_market_data::{DataProvider, MarketData};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const SH_INDEX_SECID: &str = "1.000001";

pub async fn get(market: &MarketData) -> Result<Value, String> {
    let (index_result, breadth_result) = tokio::join!(
        market.index_kline(SH_INDEX_SECID, 120),
        market.market_breadth()
    );
    let index = index_result.map_err(|error| format!("上证指数趋势数据不可用：{error}"))?;
    if index.data.len() < 61 {
        return Err(format!(
            "上证指数K线仅 {} 根，至少需要 61 根",
            index.data.len()
        ));
    }
    let closes = index.data.iter().map(|bar| bar.close).collect::<Vec<_>>();
    let last = *closes.last().ok_or_else(|| "指数K线为空".to_string())?;
    let ma20 = mean(&closes[closes.len() - 20..]);
    let ma60 = mean(&closes[closes.len() - 60..]);
    let up_days = closes[closes.len() - 21..]
        .windows(2)
        .filter(|pair| pair[1] > pair[0])
        .count();
    let up_ratio = up_days as f64 / 20.0;
    let mut score = vote(last > ma20) + vote(ma20 > ma60) + vote(up_ratio >= 0.5);
    let (breadth, breadth_error, breadth_source) = match breadth_result {
        Ok(fetched) => {
            let ratio = fetched.data.ratio();
            score += vote(ratio >= 0.5);
            (
                Some(
                    json!({"up":fetched.data.up,"down":fetched.data.down,"flat":fetched.data.flat,"total":fetched.data.total,"ratio":r4(ratio)}),
                ),
                None,
                Some(fetched.source.to_string()),
            )
        }
        Err(error) => (None, Some(error.to_string()), None),
    };
    let available = if breadth.is_some() { 4 } else { 3 };
    let regime = classify(score);
    let mut digest = Sha256::new();
    for bar in &index.data {
        digest.update(bar.date.to_string().as_bytes());
        digest.update(bar.close.to_bits().to_le_bytes());
    }
    Ok(json!({
        "regime":regime, "score":score, "available_signals":available, "expected_signals":4,
        "verification_status":if available == 4 {"complete"} else {"partial"},
        "scoring":"每项可用信号各投 ±1 票：收盘>MA20、MA20>MA60、近20日上涨占比≥0.5、涨跌家数比≥0.5；缺失信号不按零或中性补造",
        "index":{"secid":SH_INDEX_SECID,"close":r2(last),"as_of":index.data.last().map(|bar|bar.date.to_string())},
        "trend":{"ma20":r2(ma20),"ma60":r2(ma60),"above_ma20":last>ma20,"ma20_above_ma60":ma20>ma60,
            "dist_ma20_pct":r2((last-ma20)/ma20*100.0),"dist_ma60_pct":r2((last-ma60)/ma60*100.0)},
        "breadth":breadth, "breadth_error":breadth_error, "up_days_20":up_days, "up_ratio_20":r4(up_ratio),
        "source":index.source.to_string(), "breadth_source":breadth_source, "fetched_at":index.fetched_at,
        "source_version_id":format!("market-regime:{:x}",digest.finalize()),
    }))
}

fn vote(value: bool) -> i32 {
    if value {
        1
    } else {
        -1
    }
}
fn mean(values: &[f64]) -> f64 {
    values.iter().sum::<f64>() / values.len() as f64
}
fn classify(score: i32) -> &'static str {
    if score >= 2 {
        "进攻"
    } else if score <= -2 {
        "防守"
    } else {
        "中性"
    }
}
fn r2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}
fn r4(value: f64) -> f64 {
    (value * 10_000.0).round() / 10_000.0
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn thresholds_are_deterministic() {
        assert_eq!(classify(2), "进攻");
        assert_eq!(classify(-2), "防守");
        assert_eq!(classify(1), "中性");
    }
}
