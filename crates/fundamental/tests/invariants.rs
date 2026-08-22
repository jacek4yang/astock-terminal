//! Property-based invariants for the analytics modules.

use astock_fundamental::{metrics, valuation};
use proptest::prelude::*;

proptest! {
    /// Gross margin for positive revenue and non-negative cost is in
    /// (−∞, 1] — it can be arbitrarily negative (cost ≫ revenue) but never
    /// exceeds 100%.
    #[test]
    fn gross_margin_bounded(revenue in 0.01f64..1e12, cost in 0.0f64..1e13) {
        let m = metrics::gross_margin(Some(revenue), Some(cost)).unwrap();
        prop_assert!(m <= 1.0 + 1e-12, "margin {m} > 1");
        prop_assert!(m.is_finite());
    }

    /// Net/operating margins are finite whenever defined.
    #[test]
    fn other_margins_finite(revenue in 0.01f64..1e12, profit in -1e12f64..1e12) {
        let nm = metrics::net_margin(Some(profit), Some(revenue)).unwrap();
        prop_assert!(nm.is_finite());
        let om = metrics::operating_margin(Some(profit), Some(revenue)).unwrap();
        prop_assert!(om.is_finite());
    }

    /// Percentile always lands in [0, 100].
    #[test]
    fn percentile_in_range(
        history in proptest::collection::vec(-1e6f64..1e6, 0..200),
        current in -1e6f64..1e6,
    ) {
        if let Some(p) = valuation::percentile(&history, current) {
            prop_assert!((0.0..=100.0).contains(&p), "percentile {p} out of range");
        }
    }

    /// Growth is finite and sign-consistent: curr > prev > 0 ⇒ growth > 0.
    #[test]
    fn growth_sign_consistent(prev in 0.01f64..1e9, curr in 0.01f64..1e9) {
        let g = metrics::growth(Some(curr), Some(prev)).unwrap();
        prop_assert!(g.is_finite());
        prop_assert_eq!(g > 0.0, curr > prev);
    }

    /// ROE under the average convention is finite for positive equity.
    #[test]
    fn roe_finite(np in -1e9f64..1e9, e0 in 1.0f64..1e9, e1 in 1.0f64..1e9) {
        let r = metrics::roe(Some(np), Some(e0), Some(e1)).unwrap();
        prop_assert!(r.is_finite());
    }

    /// DCF per-share is positive and finite for coherent inputs, and
    /// monotone decreasing in WACC.
    #[test]
    fn dcf_positive_and_wacc_monotone(
        fcf in 1.0f64..1e6,
        wacc in 0.06f64..0.20,
        g in 0.0f64..0.04,
    ) {
        let base = valuation::DcfInputs {
            base_fcf: fcf,
            stage1_years: 5,
            stage1_growth: 0.05,
            terminal_growth: g,
            wacc,
            net_debt: 0.0,
            shares: 100.0,
        };
        let r = valuation::dcf_fcff(&base).unwrap();
        prop_assert!(r.per_share.is_finite() && r.per_share > 0.0);
        prop_assert!((0.0..=1.0).contains(&r.terminal_share));
        let mut higher = base;
        higher.wacc = (wacc + 0.02).min(0.30);
        if higher.wacc > higher.terminal_growth {
            let r2 = valuation::dcf_fcff(&higher).unwrap();
            prop_assert!(r2.per_share < r.per_share);
        }
    }
}
