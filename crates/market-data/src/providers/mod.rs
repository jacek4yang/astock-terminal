//! Upstream adapters: Tencent (primary kline), Sina (kline fallback),
//! EastMoney (kline fallback + enrichment + everything else), TDX
//! (kline/quote fallback over the TCP quote protocol), plus the optional
//! credential-gated explicit-call sources (Tushare, iwencai, JoinQuant).

pub mod cninfo_disclosure;
pub mod eastmoney;
pub mod eastmoney_f10;
pub mod em_datacenter;
pub mod finance_news;
pub mod iwencai_openapi;
pub mod joinquant;
pub mod news_ingest;
pub mod sina;
pub mod tdx_adapter;
pub mod tencent;
pub mod tushare;

pub use cninfo_disclosure::{CninfoAnnouncement, CninfoDisclosureProvider, CninfoPage};
pub use eastmoney::{EastMoney, IndustryClassified};
pub use eastmoney_f10::{EastMoneyF10, F10Report};
pub use em_datacenter::{
    BillboardRow, BlockTradeRow, BoardConsRow, BoardKind, BoardRow, BrokenPoolRow, DtPoolRow,
    EarningsPredictRow, EmDataCenter, HolderNumRow, LiftStageRow, LimitStat, MarginDailyRow,
    NoticeNode, NoticeRow, OrgSurveyRow, PrevZtPoolRow, StrongPoolRow, StrongReason, SubNewPoolRow,
    SuspendRow, ZtPoolRow,
};
pub use finance_news::{
    FinanceNewsBatch, FinanceNewsItem, FinanceNewsProvider, FINANCE_NEWS_SOURCES,
};
pub use iwencai_openapi::{IwencaiOpenApi, StockEvents, WencaiRows};
pub use joinquant::JoinQuantProvider;
pub use news_ingest::{
    ConfiguredJsonNewsProvider, JsonNewsProviderConfig, NewsCapabilities, NewsDeliveryMode,
    NewsErrorKind, NewsIngestRequest, NewsProvider, NewsProviderError, NewsProviderHealth,
    NewsTrustTier,
};
pub use sina::SinaKline;
pub use tdx_adapter::TdxProvider;
pub use tencent::TencentKline;
pub use tushare::{
    compare_qfq_golden, qfq_factor_from_adj, AdjFactorPoint, DailyBasic, QfqMismatch, TradeCalDay,
    TushareProvider, TushareTier,
};

/// Lenient float conversion matching the legacy `_to_float`: numbers pass
/// through, numeric strings parse, and `"-"` / `""` / null become `None`.
pub(crate) fn json_f64(v: &serde_json::Value) -> Option<f64> {
    match v {
        serde_json::Value::Number(n) => n.as_f64(),
        serde_json::Value::String(s) => {
            let s = s.trim();
            if s.is_empty() || s == "-" {
                None
            } else {
                s.parse::<f64>().ok()
            }
        }
        _ => None,
    }
}

/// Compute `pct` for each bar from consecutive closes (rounded to 2dp),
/// matching the legacy adapters.
pub(crate) fn fill_pct(bars: &mut [astock_core::Bar]) {
    for i in 1..bars.len() {
        let prev = bars[i - 1].close;
        if prev > 0.0 {
            let pct = (bars[i].close - prev) / prev * 100.0;
            bars[i].pct = Some((pct * 100.0).round() / 100.0);
        }
    }
}

/// Strip an optional JSONP callback wrapper: keep everything from the first
/// `{` to the last `}`. Returns the input unchanged when it already looks
/// like bare JSON, and `None` when no object braces exist at all.
pub(crate) fn strip_jsonp(body: &str) -> Option<&str> {
    let start = body.find('{')?;
    let end = body.rfind('}')?;
    if end < start {
        return None;
    }
    Some(&body[start..=end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_f64_lenient() {
        assert_eq!(json_f64(&serde_json::json!(1.5)), Some(1.5));
        assert_eq!(json_f64(&serde_json::json!(2)), Some(2.0));
        assert_eq!(json_f64(&serde_json::json!("3.25")), Some(3.25));
        assert_eq!(json_f64(&serde_json::json!("-")), None);
        assert_eq!(json_f64(&serde_json::json!("")), None);
        assert_eq!(json_f64(&serde_json::Value::Null), None);
    }

    #[test]
    fn jsonp_stripping() {
        let bare = r#"{"QuotationCodeTable":{}}"#;
        assert_eq!(strip_jsonp(bare), Some(bare));
        let wrapped = format!("jQuery1123({bare});");
        assert_eq!(strip_jsonp(&wrapped), Some(bare));
        assert_eq!(strip_jsonp("no json here"), None);
    }

    #[test]
    fn pct_from_consecutive_closes() {
        use astock_core::{Bar, VolumeUnit};
        use chrono::NaiveDate;
        let d = |day: u32| NaiveDate::from_ymd_opt(2025, 8, day).unwrap();
        let mut bars = vec![
            Bar::new(d(20), 10.0, 10.0, 10.1, 9.9, 1.0, VolumeUnit::Lots),
            Bar::new(d(21), 10.0, 10.5, 10.6, 9.9, 1.0, VolumeUnit::Lots),
        ];
        fill_pct(&mut bars);
        assert_eq!(bars[0].pct, None);
        assert_eq!(bars[1].pct, Some(5.0));
    }
}
