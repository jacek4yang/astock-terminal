//! Dimensionality reduction and clustering: PCA on the correlation or
//! covariance matrix, and seeded k-means with k-means++ initialization.

use nalgebra::DMatrix;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

use crate::error::{validate_series, QuantError};
use crate::matrix::validate_multi;

/// Matrix basis for PCA.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PcaBasis {
    /// Eigendecompose the correlation matrix (series are standardized
    /// first) — the right choice when units differ across variables.
    Correlation,
    /// Eigendecompose the sample covariance matrix (series are centered
    /// only) — preserves the natural scale of the variables.
    Covariance,
}

/// PCA result.
#[derive(Debug, Clone)]
pub struct PcaResult {
    /// Eigenvalues in descending order (variances along each component).
    pub eigenvalues: Vec<f64>,
    /// Loadings matrix: column j is the j-th principal direction
    /// (unit-norm, descending eigenvalue order).
    pub loadings: DMatrix<f64>,
    /// Share of total variance explained by each component.
    pub explained_variance_ratio: Vec<f64>,
    /// Which basis was used.
    pub basis: PcaBasis,
}

/// Principal component analysis of `p` variable series of common length
/// `n` via the symmetric eigendecomposition of the chosen matrix.
///
/// With `PcaBasis::Correlation` each series is standardized to zero mean
/// and unit *sample* variance first, so the result is scale-free; with
/// `PcaBasis::Covariance` series are only centered. Sign convention:
/// component signs are arbitrary; we canonicalize so the largest-|value|
/// loading of each column is positive, which keeps results deterministic.
pub fn pca(series: &[&[f64]], basis: PcaBasis) -> Result<PcaResult, QuantError> {
    validate_multi(series, 2, "pca")?;
    let p = series.len();
    // Build the matrix to decompose.
    let mat = match basis {
        PcaBasis::Correlation => crate::matrix::correlation_matrix(series)?,
        PcaBasis::Covariance => crate::matrix::covariance_matrix(series)?,
    };
    let eig = nalgebra::SymmetricEigen::new(mat);
    // Sort eigenpairs by descending eigenvalue.
    let mut order: Vec<usize> = (0..p).collect();
    order.sort_by(|&a, &b| eig.eigenvalues[b].total_cmp(&eig.eigenvalues[a]));
    let eigenvalues: Vec<f64> = order.iter().map(|&i| eig.eigenvalues[i]).collect();
    let total: f64 = eigenvalues.iter().sum();
    if total <= 0.0 {
        return Err(QuantError::NumericalIssue(
            "pca: zero total variance across all series".into(),
        ));
    }
    let mut loadings = DMatrix::zeros(p, p);
    for (col, &idx) in order.iter().enumerate() {
        for row in 0..p {
            loadings[(row, col)] = eig.eigenvectors[(row, idx)];
        }
        // Deterministic sign: make the largest-|loading| entry positive.
        let mut pivot = 0usize;
        for row in 1..p {
            if loadings[(row, col)].abs() > loadings[(pivot, col)].abs() {
                pivot = row;
            }
        }
        if loadings[(pivot, col)] < 0.0 {
            for row in 0..p {
                loadings[(row, col)] = -loadings[(row, col)];
            }
        }
    }
    let explained_variance_ratio = eigenvalues.iter().map(|v| v / total).collect();
    Ok(PcaResult {
        eigenvalues,
        loadings,
        explained_variance_ratio,
        basis,
    })
}

/// k-means clustering result.
#[derive(Debug, Clone)]
pub struct KMeansResult {
    /// Final centroids, k × d.
    pub centroids: Vec<Vec<f64>>,
    /// Cluster assignment (0..k) for each input point.
    pub assignments: Vec<usize>,
    /// Total within-cluster sum of squared distances of the best restart.
    pub inertia: f64,
}

/// k-means with **k-means++ initialization** and multiple restarts.
///
/// - Points are rows of `points` (each a d-dimensional vector, d ≥ 1,
///   all equal length, all finite); `n ≥ k`.
/// - k-means++ seeding: first centroid uniform at random, each next one
///   drawn with probability proportional to D(x)² (distance to the
///   nearest existing centroid).
/// - Lloyd iterations until assignments stop changing or 100 iterations.
/// - `restarts` independent runs (≥ 1), best inertia wins. The `seed`
///   makes the whole procedure exactly reproducible.
pub fn kmeans(
    points: &[Vec<f64>],
    k: usize,
    restarts: usize,
    seed: u64,
) -> Result<KMeansResult, QuantError> {
    if points.is_empty() {
        return Err(QuantError::InvalidInput("kmeans: no points given".into()));
    }
    let d = points[0].len();
    if d == 0 {
        return Err(QuantError::InvalidInput(
            "kmeans: points must have at least one dimension".into(),
        ));
    }
    for p in points {
        validate_series(p, 1, "kmeans")?;
        if p.len() != d {
            return Err(QuantError::InvalidInput(
                "kmeans: inconsistent point dimensions".into(),
            ));
        }
    }
    if k == 0 || k > points.len() {
        return Err(QuantError::InvalidInput(format!(
            "kmeans: k must be in 1..={}, got {k}",
            points.len()
        )));
    }
    if restarts == 0 {
        return Err(QuantError::InvalidInput(
            "kmeans: restarts must be >= 1".into(),
        ));
    }
    let mut rng = StdRng::seed_from_u64(seed);
    let mut best: Option<KMeansResult> = None;
    for _ in 0..restarts {
        let mut centroids = kmeanspp(points, k, &mut rng);
        let (assignments, inertia) = lloyd(points, &mut centroids);
        let result = KMeansResult {
            centroids,
            assignments,
            inertia,
        };
        if best.as_ref().is_none_or(|b| result.inertia < b.inertia) {
            best = Some(result);
        }
    }
    Ok(best.expect("at least one restart ran"))
}

/// k-means++ centroid seeding.
fn kmeanspp(points: &[Vec<f64>], k: usize, rng: &mut StdRng) -> Vec<Vec<f64>> {
    let n = points.len();
    let mut centroids: Vec<Vec<f64>> = Vec::with_capacity(k);
    let first = rng.random_range(0..n);
    centroids.push(points[first].clone());
    let mut d2: Vec<f64> = points.iter().map(|p| sq_dist(p, &centroids[0])).collect();
    while centroids.len() < k {
        let total: f64 = d2.iter().sum();
        if total <= 0.0 {
            // All remaining points coincide with existing centroids;
            // pick deterministically.
            centroids.push(points[centroids.len() % n].clone());
            continue;
        }
        let mut draw = rng.random::<f64>() * total;
        let mut chosen = n - 1;
        for (i, w) in d2.iter().enumerate() {
            draw -= w;
            if draw <= 0.0 {
                chosen = i;
                break;
            }
        }
        centroids.push(points[chosen].clone());
        for (i, p) in points.iter().enumerate() {
            let nd = sq_dist(p, &points[chosen]);
            if nd < d2[i] {
                d2[i] = nd;
            }
        }
    }
    centroids
}

fn sq_dist(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b).map(|(x, y)| (x - y) * (x - y)).sum()
}

/// Lloyd's algorithm; returns final assignments and inertia.
fn lloyd(points: &[Vec<f64>], centroids: &mut [Vec<f64>]) -> (Vec<usize>, f64) {
    let k = centroids.len();
    let d = centroids[0].len();
    let mut assignments = vec![usize::MAX; points.len()];
    for _ in 0..100 {
        let mut changed = false;
        for (i, p) in points.iter().enumerate() {
            let mut best_c = 0;
            let mut best_d = f64::INFINITY;
            for (c, cen) in centroids.iter().enumerate() {
                let dist = sq_dist(p, cen);
                if dist < best_d {
                    best_d = dist;
                    best_c = c;
                }
            }
            if assignments[i] != best_c {
                assignments[i] = best_c;
                changed = true;
            }
        }
        if !changed {
            break;
        }
        // Recompute centroids; empty clusters keep their old centroid.
        let mut sums = vec![vec![0.0; d]; k];
        let mut counts = vec![0usize; k];
        for (p, &a) in points.iter().zip(&assignments) {
            counts[a] += 1;
            for (dim, &value) in p.iter().enumerate().take(d) {
                sums[a][dim] += value;
            }
        }
        for c in 0..k {
            if counts[c] > 0 {
                for dim in 0..d {
                    centroids[c][dim] = sums[c][dim] / counts[c] as f64;
                }
            }
        }
    }
    let inertia = points
        .iter()
        .zip(&assignments)
        .map(|(p, &a)| sq_dist(p, &centroids[a]))
        .sum();
    (assignments, inertia)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pca_first_component_at_45_degrees() {
        // Two perfectly correlated variables, standardized: the
        // correlation matrix is [[1, 1], [1, 1]] with eigenvectors
        // (1,1)/√2 (λ = 2) and (1,-1)/√2 (λ = 0) — i.e. PC1 at exactly
        // 45°, explaining 100% of the variance.
        let x: Vec<f64> = (0..20).map(|i| i as f64).collect();
        let y: Vec<f64> = x.iter().map(|v| 3.0 * v + 7.0).collect();
        let res = pca(&[&x, &y], PcaBasis::Correlation).unwrap();
        let inv_sqrt2 = 1.0 / 2.0_f64.sqrt();
        assert!((res.loadings[(0, 0)] - inv_sqrt2).abs() < 1e-10);
        assert!((res.loadings[(1, 0)] - inv_sqrt2).abs() < 1e-10);
        assert!((res.eigenvalues[0] - 2.0).abs() < 1e-10);
        assert!((res.explained_variance_ratio[0] - 1.0).abs() < 1e-10);
    }

    #[test]
    fn pca_covariance_golden_two_var() {
        // x = [1,2,3], y = [1,3,2] (from matrix tests): cov = [[1, 0.5],
        // [0.5, 1]] → eigenvalues 1.5 and 0.5, PC1 direction (1,1)/√2.
        let res = pca(&[&[1.0, 2.0, 3.0], &[1.0, 3.0, 2.0]], PcaBasis::Covariance).unwrap();
        assert!((res.eigenvalues[0] - 1.5).abs() < 1e-10);
        assert!((res.eigenvalues[1] - 0.5).abs() < 1e-10);
        assert!((res.explained_variance_ratio[0] - 0.75).abs() < 1e-10);
        assert!((res.loadings[(0, 0)] - res.loadings[(1, 0)]).abs() < 1e-10);
    }

    #[test]
    fn kmeans_separates_obvious_clusters() {
        // Two clusters far apart: near (0,0) and near (10,10).
        let mut points = Vec::new();
        for i in 0..20 {
            let f = i as f64 * 0.01;
            points.push(vec![f, -f]);
            points.push(vec![10.0 + f, 10.0 - f]);
        }
        let res = kmeans(&points, 2, 5, 42).unwrap();
        // Every low point must share a cluster with every other low point.
        let a0 = res.assignments[0];
        for (i, &a) in res.assignments.iter().enumerate() {
            let is_low = i % 2 == 0;
            assert_eq!(a == a0, is_low, "point {i} mis-assigned");
        }
        // Centroids near the true centers (0,0) and (10,10).
        let c = &res.centroids[a0];
        assert!(c[0].abs() < 0.2 && c[1].abs() < 0.2, "centroid = {c:?}");
    }

    #[test]
    fn kmeans_deterministic_given_seed() {
        let points: Vec<Vec<f64>> = (0..30).map(|i| vec![i as f64, (i % 7) as f64]).collect();
        let a = kmeans(&points, 3, 4, 7).unwrap();
        let b = kmeans(&points, 3, 4, 7).unwrap();
        assert_eq!(a.assignments, b.assignments);
        assert_eq!(a.inertia, b.inertia);
    }

    #[test]
    fn rejects_bad_input() {
        assert!(matches!(
            pca(&[&[1.0, 2.0], &[1.0, 2.0, 3.0]], PcaBasis::Correlation),
            Err(QuantError::InvalidInput(_))
        ));
        let pts = vec![vec![1.0], vec![2.0]];
        assert!(matches!(
            kmeans(&pts, 5, 1, 0),
            Err(QuantError::InvalidInput(_))
        ));
    }
}
