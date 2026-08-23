//! Manual live smoke for the issue #14 dual-source quality gate.
//!
//! Kept ignored so deterministic CI never treats third-party availability as
//! product correctness. Run explicitly with network access.

use astock_core::{
    reconcile_numeric, AccountingScope, AdjustmentBasis, Currency, DataUnit, NumericObservation,
    ReconciliationStatus, ReconciliationTolerance, Symbol,
};
use astock_market_data::{DataProvider, MarketData};

#[tokio::test]
#[ignore = "live dual-source quote check: requires TDX TCP and EastMoney network access"]
async fn quote_600519_has_two_real_sources_and_comparable_price() {
    let market = MarketData::new();
    let symbol = Symbol::new("600519").unwrap();
    let (tdx, eastmoney) = tokio::join!(market.tdx.quote(&symbol), market.eastmoney.quote(&symbol));
    let tdx = tdx.expect("TDX live quote");
    let eastmoney = eastmoney.expect("EastMoney live quote");
    assert_eq!(tdx.data.symbol, "600519");
    assert_eq!(eastmoney.data.symbol, "600519");
    let observation = |provider: &str, value: f64| NumericObservation {
        provider: provider.into(),
        field: "price".into(),
        value,
        unit: DataUnit::Price,
        currency: Some(Currency::Cny),
        adjustment: AdjustmentBasis::None,
        accounting_scope: AccountingScope::NotApplicable,
        as_of_time: None,
    };
    let result = reconcile_numeric(
        observation("tdx", tdx.data.price),
        observation("eastmoney", eastmoney.data.price),
        ReconciliationTolerance {
            absolute: 0.01,
            relative: 0.002,
        },
    );
    assert!(matches!(
        result.status,
        ReconciliationStatus::Matched | ReconciliationStatus::WithinTolerance
    ));
}
