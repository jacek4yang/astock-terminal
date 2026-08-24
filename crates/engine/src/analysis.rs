use astock_core::{Bar, FundFlowPoint, MarketBreadth, Quote as CoreQuote, Symbol};
use astock_market_data::{DataProvider, MarketData};
use astock_technical::{Breadth, FundFlow, Kline, Quote};
use astock_trading_rules::RuleSet;
use serde_json::Value;

fn bars_to_klines(bars: &[Bar]) -> Vec<Kline> {
    bars.iter()
        .map(|bar| Kline {
            date: bar.date.to_string(),
            open: bar.open,
            close: bar.close,
            high: bar.high,
            low: bar.low,
            volume: bar.volume,
            amount: bar.amount.unwrap_or(0.0),
            pct: bar.pct.unwrap_or(0.0),
            turnover: bar.turnover.unwrap_or(0.0),
        })
        .collect()
}

fn quote_to_technical(quote: &CoreQuote) -> Quote {
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
        turnover: quote.turnover.unwrap_or(0.0),
        timestamp: quote.timestamp.to_rfc3339(),
    }
}

fn flows_to_technical(flows: &[FundFlowPoint]) -> Vec<FundFlow> {
    flows
        .iter()
        .map(|flow| FundFlow {
            date: flow.time.date().to_string(),
            main_net: flow.main_net,
            super_large_net: flow.super_large_net,
            large_net: flow.large_net,
            medium_net: flow.medium_net,
            small_net: flow.small_net,
            main_pct: flow.main_pct,
        })
        .collect()
}

fn breadth_to_technical(value: &MarketBreadth) -> Breadth {
    Breadth {
        up: i64::from(value.up),
        down: i64::from(value.down),
        flat: i64::from(value.flat),
        total: i64::from(value.total),
        breadth_ratio: value.ratio(),
    }
}

pub async fn signal(
    market: &MarketData,
    rules: &RuleSet,
    symbol: &Symbol,
    bars: &[Bar],
    quote: &CoreQuote,
    flows: Option<&[FundFlowPoint]>,
    source: &str,
) -> Value {
    let (index, breadth) =
        tokio::join!(market.index_kline("1.000001", 60), market.market_breadth());
    let index = index.ok().map(|value| bars_to_klines(&value.data));
    let breadth = breadth.ok().map(|value| breadth_to_technical(&value.data));
    let klines = bars_to_klines(bars);
    let technical_quote = quote_to_technical(quote);
    let technical_flows = flows.map(flows_to_technical);
    let mut result = astock_technical::analyze(
        &klines,
        Some(&technical_quote),
        technical_flows.as_deref(),
        index.as_deref(),
        breadth.as_ref(),
    );
    attach_manual_plan(&mut result, rules, symbol, quote, &klines, source);
    result
}

pub fn chanlun(symbol: &Symbol, bars: &[Bar]) -> Result<Value, String> {
    if bars.is_empty() {
        return Err(format!("no kline data for {symbol}"));
    }
    let dates = bars
        .iter()
        .map(|bar| bar.date.to_string())
        .collect::<Vec<_>>();
    let opens = bars.iter().map(|bar| bar.open).collect::<Vec<_>>();
    let closes = bars.iter().map(|bar| bar.close).collect::<Vec<_>>();
    let highs = bars.iter().map(|bar| bar.high).collect::<Vec<_>>();
    let lows = bars.iter().map(|bar| bar.low).collect::<Vec<_>>();
    let volumes = bars.iter().map(|bar| bar.volume).collect::<Vec<_>>();
    let result = astock_chanlun::daily::analyze_chanlun_daily(
        &dates, &opens, &closes, &highs, &lows, &volumes,
    );
    Ok(astock_chanlun::daily::daily_result_to_dict(&result))
}

fn attach_manual_plan(
    signal: &mut Value,
    rules: &RuleSet,
    symbol: &Symbol,
    quote: &CoreQuote,
    klines: &[Kline],
    source: &str,
) {
    let auction = &rules.data.auction;
    let sessions = astock_technical::SessionSchedule {
        open_auction_start: auction.open_call_auction.start.clone(),
        open_auction_end: auction.open_call_auction.end.clone(),
        morning_start: auction.continuous_morning.start.clone(),
        morning_end: auction.continuous_morning.end.clone(),
        afternoon_start: auction.continuous_afternoon.start.clone(),
        afternoon_end: auction.continuous_afternoon.end.clone(),
        close_auction_start: auction.close_call_auction.start.clone(),
        close_auction_end: auction.close_call_auction.end.clone(),
    };
    let board = rules.for_symbol(symbol.code()).ok();
    let constraints = astock_technical::TradingConstraints {
        board_name: board
            .as_ref()
            .map_or_else(|| "未知板块".to_string(), |value| value.board_name.clone()),
        price_limit_pct: board
            .as_ref()
            .map_or(0.10, |value| value.price_limit_pct(false)),
        min_lot: board.as_ref().map_or(100, |value| value.min_lot),
        lot_step: board.as_ref().map_or(100, |value| value.lot_step),
        t_plus_1: board.as_ref().is_none_or(|value| value.t_plus_1),
    };
    let generated_at = astock_core::time::utc_now().to_rfc3339();
    let Some(plan) = astock_technical::build_manual_trading_plan(
        symbol.code(),
        &quote.name,
        klines,
        signal,
        &sessions,
        &constraints,
        &generated_at,
        source,
    ) else {
        return;
    };
    let Ok(plan_json) = serde_json::to_value(&plan) else {
        return;
    };
    if let Some(object) = signal.as_object_mut() {
        object.insert("manual_plan".to_string(), plan_json);
        object.insert(
            "plain_summary".to_string(),
            Value::String(format!(
                "{}；反方条件：{}。本方案只在检查点条件成立时供人工执行。",
                plan.thesis, plan.counter_thesis
            )),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use astock_core::VolumeUnit;
    use chrono::NaiveDate;

    #[test]
    fn bar_conversion_preserves_market_fields() {
        let rows = bars_to_klines(&[Bar {
            date: NaiveDate::from_ymd_opt(2026, 8, 24).unwrap(),
            open: 10.0,
            close: 10.5,
            high: 10.8,
            low: 9.9,
            volume: 123.0,
            volume_unit: VolumeUnit::Lots,
            amount: Some(4_000.0),
            pct: Some(5.0),
            turnover: Some(1.2),
        }]);
        assert_eq!(rows[0].close, 10.5);
        assert_eq!(rows[0].turnover, 1.2);
    }
}
