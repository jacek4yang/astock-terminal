//! Property-based invariants for astock-quant (proptest).

use astock_quant::correlation::{kendall_tau_b, pearson, spearman};
use astock_quant::matrix::{correlation_matrix, nearest_psd};
use astock_quant::returns::{arithmetic_returns, log_returns};
use astock_quant::simulation::historical_var_es;
use proptest::prelude::*;

/// Two equal-length finite series with values in a bounded range.
fn pair_strategy() -> impl Strategy<Value = (Vec<f64>, Vec<f64>)> {
    (2..=60usize).prop_flat_map(|n| {
        (
            proptest::collection::vec(-100.0f64..100.0, n),
            proptest::collection::vec(-100.0f64..100.0, n),
        )
    })
}

/// Three equal-length finite series.
fn triple_strategy() -> impl Strategy<Value = (Vec<f64>, Vec<f64>, Vec<f64>)> {
    (2..=60usize).prop_flat_map(|n| {
        (
            proptest::collection::vec(-100.0f64..100.0, n),
            proptest::collection::vec(-100.0f64..100.0, n),
            proptest::collection::vec(-100.0f64..100.0, n),
        )
    })
}

fn has_variance(x: &[f64]) -> bool {
    let m = x.iter().sum::<f64>() / x.len() as f64;
    x.iter().any(|v| (*v - m).abs() > 1e-9)
}

proptest! {
    #[test]
    fn pearson_within_unit_bounds((x, y) in pair_strategy()) {
        prop_assume!(has_variance(&x) && has_variance(&y));
        let r = pearson(&x, &y).unwrap();
        prop_assert!((-1.0..=1.0).contains(&r), "pearson = {r}");
    }

    #[test]
    fn spearman_within_unit_bounds((x, y) in pair_strategy()) {
        prop_assume!(has_variance(&x) && has_variance(&y));
        let rho = spearman(&x, &y).unwrap();
        prop_assert!((-1.0..=1.0).contains(&rho), "spearman = {rho}");
    }

    #[test]
    fn kendall_within_unit_bounds((x, y) in pair_strategy()) {
        prop_assume!(has_variance(&x) && has_variance(&y));
        let tau = kendall_tau_b(&x, &y).unwrap();
        prop_assert!((-1.0..=1.0).contains(&tau), "kendall = {tau}");
    }

    #[test]
    fn returns_length_invariant(prices in proptest::collection::vec(0.01f64..1000.0, 2..=60)) {
        let n = prices.len();
        let ar = arithmetic_returns(&prices).unwrap();
        let lr = log_returns(&prices).unwrap();
        prop_assert_eq!(ar.len(), n - 1);
        prop_assert_eq!(lr.len(), n - 1);
        prop_assert!(ar.iter().all(|v| v.is_finite()));
        prop_assert!(lr.iter().all(|v| v.is_finite()));
    }

    #[test]
    fn cleaned_correlation_matrix_is_symmetric_psd((a, b, c) in triple_strategy()) {
        prop_assume!(has_variance(&a) && has_variance(&b) && has_variance(&c));
        let m = correlation_matrix(&[&a, &b, &c]).unwrap();
        // Perturb off-diagonals slightly to simulate estimation noise,
        // then repair: the result must be symmetric PSD.
        let mut noisy = m;
        noisy[(0, 1)] += 0.01;
        noisy[(1, 2)] -= 0.02;
        let fixed = nearest_psd(&noisy, 0.0).unwrap();
        for i in 0..3 {
            for j in 0..3 {
                prop_assert!((fixed[(i, j)] - fixed[(j, i)]).abs() < 1e-10);
            }
        }
        let eig = nalgebra::SymmetricEigen::new(fixed);
        for v in eig.eigenvalues.iter() {
            prop_assert!(*v >= -1e-10, "negative eigenvalue {v}");
        }
    }

    #[test]
    fn var_quantile_monotonicity(rets in proptest::collection::vec(-0.2f64..0.2, 20..=200)) {
        // Higher confidence ⇒ VaR (and ES) non-decreasing.
        let v90 = historical_var_es(&rets, 0.90).unwrap();
        let v95 = historical_var_es(&rets, 0.95).unwrap();
        let v99 = historical_var_es(&rets, 0.99).unwrap();
        prop_assert!(v95.var >= v90.var - 1e-12);
        prop_assert!(v99.var >= v95.var - 1e-12);
        prop_assert!(v95.es >= v95.var - 1e-12, "ES must dominate VaR");
        prop_assert!(v99.es >= v95.es - 1e-12);
    }
}
