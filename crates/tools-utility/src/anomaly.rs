//! Simplified isolation-based anomaly score.
#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_lossless,
    // `min == max` on Welford-style degenerate-input test.
    clippy::float_cmp
)]
//!
//! Not a full isolation-forest implementation — that's a substantial
//! ML algorithm with multi-tree ensembles, sub-sampling, and a
//! published `c(n)` normalisation constant. This module ships the
//! single-tree variant useful for "here are the outliers in this
//! univariate series" lookups:
//!
//!   1. Sort the input by value.
//!   2. For each point compute its "isolation cost": the depth at
//!      which a recursive median-split would isolate it.
//!   3. Normalise so the most-isolated point gets `1.0` and the
//!      least-isolated gets `0.0`.
//!
//! For production use against multi-variate datasets a dedicated ML
//! library (`linfa-trees` etc.) is the right tool; this helper is
//! deliberately scoped to the "highlight unusual observations in a
//! univariate time series" use case the v1 MCP tools cover. The
//! tool description points users at the constraint so they're not
//! surprised.

/// Per-point anomaly scores in input order. `None` for an empty
/// slice, or when all values are equal (no anomalies to detect).
/// Higher score ⇒ more anomalous.
pub fn isolation_scores(values: &[f64]) -> Option<Vec<f64>> {
    if values.is_empty() {
        return None;
    }
    let n = values.len();
    if n == 1 {
        return Some(vec![0.0]);
    }
    let mut sorted_with_idx: Vec<(usize, f64)> = values.iter().copied().enumerate().collect();
    // Total-order sort — `partial_cmp(...).unwrap_or(Equal)` would
    // silently lie about NaN comparisons. `total_cmp` defines a
    // total order over every f64 bit pattern, so direct Rust
    // callers passing NaN don't break the sort invariant.
    sorted_with_idx.sort_by(|a, b| a.1.total_cmp(&b.1));
    let min = sorted_with_idx.first().unwrap().1;
    let max = sorted_with_idx.last().unwrap().1;
    if min == max {
        return None;
    }
    // Recursive isolation depth: how many median splits are needed
    // to isolate a value. Closed-form for already-sorted data uses
    // `min(r, n - 1 - r)` — the distance from the NEAREST edge. A
    // point at the edge (rank 0 or n-1) has distance 0 → bucket 1
    // → depth 1 (most anomalous); a point at the median has the
    // largest distance → deepest bucket → highest depth (least
    // anomalous). The score is then `1 - normalised_depth` so
    // edges map to 1.0 and the median to 0.0.
    let mut raw_depths = vec![0_u32; n];
    for (rank, (orig_idx, _)) in sorted_with_idx.iter().enumerate() {
        let dist_from_edge = rank.min(n - 1 - rank);
        let bucket = dist_from_edge + 1;
        raw_depths[*orig_idx] = (bucket as f64).log2().ceil() as u32 + 1;
    }
    let max_depth = *raw_depths.iter().max().unwrap();
    let min_depth = *raw_depths.iter().min().unwrap();
    if max_depth == min_depth {
        // Every point landed at the same depth — for `n == 2` this
        // means both points are simultaneously at the edges (rank 0
        // and rank 1, both dist_from_edge = 0). The module docs say
        // the most-isolated points normalise to 1.0; with everyone
        // equally maximally isolated, that's `[1.0; n]`, not the
        // zeros the earlier code returned.
        return Some(vec![1.0; n]);
    }
    let range = (max_depth - min_depth) as f64;
    let scores: Vec<f64> = raw_depths
        .iter()
        .map(|&d| 1.0 - (d - min_depth) as f64 / range)
        .collect();
    Some(scores)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_is_none() {
        assert!(isolation_scores(&[]).is_none());
    }

    #[test]
    fn single_point_zero() {
        let s = isolation_scores(&[42.0]).unwrap();
        assert_eq!(s, vec![0.0]);
    }

    #[test]
    fn all_equal_is_none() {
        assert!(isolation_scores(&[3.0, 3.0, 3.0]).is_none());
    }

    #[test]
    fn outlier_has_highest_score() {
        // Cluster around 0, single outlier at 100.
        let mut v = vec![0.0; 9];
        v.push(100.0);
        let s = isolation_scores(&v).unwrap();
        // The outlier (index 9) should have the highest score.
        let max_idx = s
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .unwrap()
            .0;
        assert_eq!(max_idx, 9);
    }
}
