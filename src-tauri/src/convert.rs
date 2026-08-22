//! Type converters from `astock-core` market models to the
//! `astock-technical` engine input model (`Kline` / `Quote` / `FundFlow` /
//! `Breadth`). Pure functions; unit-tested without a Tauri runtime.

use astock_core::{Bar, FundFlowPoint, MarketBreadth, Quote as CoreQuote};
use astock_technical::{Breadth, FundFlow, Kline, Quote};

/// Core bar → technical kline. Missing optional fields default to 0,
/// matching the legacy engine's expectations.
pub fn bar_to_kline(bar: &Bar) -> Kline {
    Kline {
        date: bar.date.to_string(),
        open: bar.open,
        close: bar.close,
        high: bar.high,
        low: bar.low,
        volume: bar.volume,
        amount: bar.amount.unwrap_or(0.0),
        pct: bar.pct.unwrap_or(0.0),
        turnover: bar.turnover.unwrap_or(0.0),
    }
}

/// Convert a whole bar series (order preserved — ascending by date).
pub fn bars_to_klines(bars: &[Bar]) -> Vec<Kline> {
    bars.iter().map(bar_to_kline).collect()
}

/// Core quote → technical quote. The timestamp becomes RFC 3339 text.
pub fn quote_to_technical(quote: &CoreQuote) -> Quote {
    Quote {
        symbol: quote.symbol.clone(),
        name: quote.name.clone(),
        price: quote.price,
        pct: quote.pct,
        change: quote.change,
        high: quote.high,
        low: quote.low,
        open: quote.open,
        pre_close: quote.pre_close,
        volume: quote.volume,
        amount: quote.amount,
        turnover: quote.turnover,
        timestamp: quote.timestamp.to_rfc3339(),
    }
}

/// Core fund-flow point → technical fund flow. `time` is truncated to its
/// date part (daily flows are midnight timestamps by convention).
pub fn flow_to_technical(flow: &FundFlowPoint) -> FundFlow {
    FundFlow {
        date: flow.time.date().to_string(),
        main_net: flow.main_net,
        super_large_net: flow.super_large_net,
        large_net: flow.large_net,
        medium_net: flow.medium_net,
        small_net: flow.small_net,
        main_pct: flow.main_pct,
    }
}

/// Convert a fund-flow series.
pub fn flows_to_technical(flows: &[FundFlowPoint]) -> Vec<FundFlow> {
    flows.iter().map(flow_to_technical).collect()
}

/// Core market breadth → technical breadth snapshot (with the legacy ratio).
pub fn breadth_to_technical(breadth: &MarketBreadth) -> Breadth {
    Breadth {
        up: i64::from(breadth.up),
        down: i64::from(breadth.down),
        flat: i64::from(breadth.flat),
        total: i64::from(breadth.total),
        breadth_ratio: breadth.ratio(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use astock_core::VolumeUnit;
    use chrono::{NaiveDate, NaiveDateTime, TimeZone, Utc};

    fn bar() -> Bar {
        Bar {
            date: NaiveDate::from_ymd_opt(2025, 1, 2).unwrap(),
            open: 10.0,
            close: 10.5,
            high: 10.8,
            low: 9.9,
            volume: 12_345.0,
            volume_unit: VolumeUnit::Lots,
            amount: Some(1.3e8),
            turnover: None,
            pct: Some(1.5),
        }
    }

    #[test]
    fn bar_conversion_maps_optionals() {
        let k = bar_to_kline(&bar());
        assert_eq!(k.date, "2025-01-02");
        assert_eq!(k.close, 10.5);
        assert_eq!(k.amount, 1.3e8);
        assert_eq!(k.pct, 1.5);
        assert_eq!(k.turnover, 0.0); // None -> 0
    }

    #[test]
    fn quote_conversion_keeps_fields() {
        let q = CoreQuote {
            symbol: "600519".into(),
            name: "贵州茅台".into(),
            price: 1800.0,
            open: 1790.0,
            high: 1810.0,
            low: 1780.0,
            pre_close: 1795.0,
            volume: 1000.0,
            amount: 1.8e6,
            change: 5.0,
            pct: 0.28,
            turnover: 0.3,
            timestamp: Utc.with_ymd_and_hms(2025, 1, 2, 7, 0, 0).unwrap(),
        };
        let t = quote_to_technical(&q);
        assert_eq!(t.symbol, "600519");
        assert_eq!(t.pre_close, 1795.0);
        assert!(t.timestamp.starts_with("2025-01-02T07:00:00"));
    }

    #[test]
    fn flow_conversion_truncates_to_date() {
        let f = FundFlowPoint {
            time: NaiveDateTime::parse_from_str("2025-01-02 00:00:00", "%Y-%m-%d %H:%M:%S")
                .unwrap(),
            main_net: 1.0,
            small_net: 2.0,
            medium_net: 3.0,
            large_net: 4.0,
            super_large_net: 5.0,
            main_pct: 6.0,
        };
        let t = flow_to_technical(&f);
        assert_eq!(t.date, "2025-01-02");
        assert_eq!(t.super_large_net, 5.0);
        assert_eq!(t.main_pct, 6.0);
    }

    #[test]
    fn breadth_ratio_matches_legacy() {
        let b = MarketBreadth {
            up: 3,
            down: 1,
            flat: 0,
            total: 4,
        };
        let t = breadth_to_technical(&b);
        assert_eq!(t.breadth_ratio, 0.75);
        let flat = MarketBreadth {
            up: 0,
            down: 0,
            flat: 10,
            total: 10,
        };
        assert_eq!(breadth_to_technical(&flat).breadth_ratio, 0.5);
    }
}
