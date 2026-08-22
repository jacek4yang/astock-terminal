//! Determinism guarantees: identical seeds must reproduce identical
//! outputs bit-for-bit across all stochastic routines.

use astock_quant::dimred::kmeans;
use astock_quant::leadlag::leadlag_bootstrap_pvalue;
use astock_quant::simulation::{block_bootstrap, gbm_paths, mc_var_es, stationary_bootstrap};

#[test]
fn gbm_paths_reproduce_exactly() {
    let a = gbm_paths(100.0, 0.08, 0.25, 1.0 / 252.0, 100, 8, 777).unwrap();
    let b = gbm_paths(100.0, 0.08, 0.25, 1.0 / 252.0, 100, 8, 777).unwrap();
    assert_eq!(a, b);
}

#[test]
fn bootstraps_reproduce_exactly() {
    let x: Vec<f64> = (0..80).map(|i| (i as f64 * 0.2).sin()).collect();
    assert_eq!(
        stationary_bootstrap(&x, 4.0, 200, 55).unwrap(),
        stationary_bootstrap(&x, 4.0, 200, 55).unwrap()
    );
    assert_eq!(
        block_bootstrap(&x, 6, 200, 56).unwrap(),
        block_bootstrap(&x, 6, 200, 56).unwrap()
    );
    // Different seeds differ.
    assert_ne!(
        stationary_bootstrap(&x, 4.0, 200, 55).unwrap(),
        stationary_bootstrap(&x, 4.0, 200, 56).unwrap()
    );
}

#[test]
fn mc_var_es_reproduces_exactly() {
    let a = mc_var_es(0.001, 0.02, 0.99, 10_000, 3).unwrap();
    let b = mc_var_es(0.001, 0.02, 0.99, 10_000, 3).unwrap();
    assert_eq!(a, b);
}

#[test]
fn kmeans_reproduces_exactly() {
    let points: Vec<Vec<f64>> = (0..40)
        .map(|i| vec![(i % 10) as f64, (i / 10) as f64])
        .collect();
    let a = kmeans(&points, 3, 5, 11).unwrap();
    let b = kmeans(&points, 3, 5, 11).unwrap();
    assert_eq!(a.assignments, b.assignments);
    assert_eq!(a.centroids, b.centroids);
}

#[test]
fn leadlag_bootstrap_reproduces_exactly() {
    let x: Vec<f64> = (0..60).map(|i| (i as f64 * 0.45).cos()).collect();
    let y: Vec<f64> = (0..60).map(|i| (i as f64 * 0.31).sin()).collect();
    let a = leadlag_bootstrap_pvalue(&x, &y, 1, 4, 199, 99).unwrap();
    let b = leadlag_bootstrap_pvalue(&x, &y, 1, 4, 199, 99).unwrap();
    assert_eq!(a, b);
}
