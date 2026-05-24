//! Pure-math statistics shared by the seven `stats_*` MCP tool
//! wrappers and the two `series_*` tools. No async, no I/O —
//! plain functions over `&[f64]` so Rust callers can use them
//! directly without the MCP serde round-trip.
//!
//! Algorithmic choices favour the textbook "do the obvious thing"
//! over micro-optimisation: each helper is read once during a tool
//! call, the inputs are bounded by the tool input-schema max-length,
//! and `f64` precision is fine for the use cases the v1 utility
//! tools cover. Where the obvious algorithm is numerically iffy
//! (e.g. naïve variance), we use a one-pass Welford variant.
//!
//! Numeric-cast lints are silenced module-wide because the math is
//! peppered with `usize` ↔ `f64` and `f64` ↔ `usize` round-trips
//! (length → divisor, percentile rank → array index, etc.). The
//! input arrays are length-capped by the tool wrappers at 100k —
//! comfortably under f64's 2^53 mantissa — so the precision loss
//! the lint warns about cannot happen in practice. The
//! `cast_possible_truncation` + `cast_sign_loss` pair only fires
//! after explicit bounds checks (rank is in `[0, n-1]` after the
//! caller validated `p ∈ [0, 100]`).
#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    // `min == max` etc. — for "degenerate input where every value
    // is identical" we want exact equality, not "within epsilon".
    // The two values originate from the same iteration so a
    // bit-for-bit compare is the correct test.
    clippy::float_cmp,
    clippy::cast_lossless,
    // `for i in 0..n` indexed loops are intentional in tight inner
    // hot paths where the iterator chain is harder to follow.
    clippy::needless_range_loop,
    clippy::explicit_iter_loop
)]

/// Population summary of a non-empty slice. Returns `None` for any
/// of: (a) empty slice, (b) any input is non-finite (NaN / ±∞).
/// Tool wrappers turn `None` into an `InvalidArguments` response
/// so callers learn which precondition failed.
///
/// All values are population statistics (divisor `n`), not sample
/// (divisor `n - 1`). That matches the textbook "describe what's in
/// front of you" framing — sample statistics are an inferential
/// step we don't want to bake in.
pub fn summary(values: &[f64]) -> Option<Summary> {
    if values.is_empty() {
        return None;
    }
    // Reject non-finite inputs explicitly so direct Rust callers
    // (the module is `pub`-exported, not gated through MCP) can't
    // poison the Welford accumulator with NaN/±∞. Mirrors what
    // `histogram` already does for consistency.
    if !values.iter().all(|v| v.is_finite()) {
        return None;
    }
    // Welford's online variance — numerically stable; sums the
    // running mean + M₂ in one pass.
    let mut mean = 0.0;
    let mut m2 = 0.0;
    let mut min = f64::INFINITY;
    let mut max = f64::NEG_INFINITY;
    for (i, &x) in values.iter().enumerate() {
        let n = (i + 1) as f64;
        let delta = x - mean;
        mean += delta / n;
        let delta2 = x - mean;
        m2 += delta * delta2;
        if x < min {
            min = x;
        }
        if x > max {
            max = x;
        }
    }
    let n_f = values.len() as f64;
    let variance = m2 / n_f;
    Some(Summary {
        count: values.len(),
        mean,
        median: median(values),
        variance,
        std_dev: variance.sqrt(),
        min,
        max,
        sum: mean * n_f,
    })
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Summary {
    pub count: usize,
    pub mean: f64,
    pub median: f64,
    pub variance: f64,
    pub std_dev: f64,
    pub min: f64,
    pub max: f64,
    pub sum: f64,
}

fn median(values: &[f64]) -> f64 {
    let mut sorted: Vec<f64> = values.to_vec();
    // `f64::total_cmp` defines a total order over ALL f64 bit
    // patterns (including NaN); `partial_cmp(...).unwrap_or(Equal)`
    // would silently lie about NaN comparisons and break the
    // sort invariant. Pure helpers are pub-exported so a direct
    // Rust caller might pass NaN.
    sorted.sort_by(f64::total_cmp);
    let n = sorted.len();
    if n == 0 {
        f64::NAN
    } else if n % 2 == 1 {
        sorted[n / 2]
    } else {
        0.5 * (sorted[n / 2 - 1] + sorted[n / 2])
    }
}

/// Linear-interpolation percentile (`NumPy` `np.percentile` default
/// mode, `interpolation="linear"`). `p` is in `[0, 100]`.
pub fn percentile(values: &[f64], p: f64) -> Option<f64> {
    if values.is_empty() || !(0.0..=100.0).contains(&p) {
        return None;
    }
    let mut sorted: Vec<f64> = values.to_vec();
    // Total-order sort — see `median` for the NaN-safety rationale.
    sorted.sort_by(f64::total_cmp);
    let n = sorted.len();
    if n == 1 {
        return Some(sorted[0]);
    }
    let rank = (p / 100.0) * (n as f64 - 1.0);
    let lo = rank.floor() as usize;
    let hi = rank.ceil() as usize;
    if lo == hi {
        Some(sorted[lo])
    } else {
        let frac = rank - lo as f64;
        Some(sorted[lo] * (1.0 - frac) + sorted[hi] * frac)
    }
}

/// Equal-width histogram with `bins` buckets. Returns counts per
/// bucket plus the bucket boundaries. Values exactly equal to the
/// upper edge fall in the LAST bucket (consistent with `np.histogram`
/// — otherwise the maximum value would have no home).
pub fn histogram(values: &[f64], bins: usize) -> Option<Histogram> {
    if values.is_empty() || bins == 0 {
        return None;
    }
    let mut min = f64::INFINITY;
    let mut max = f64::NEG_INFINITY;
    for &v in values {
        if !v.is_finite() {
            return None;
        }
        if v < min {
            min = v;
        }
        if v > max {
            max = v;
        }
    }
    let span = max - min;
    let edges: Vec<f64> = if span == 0.0 {
        // Degenerate: all values equal. One bin covering [v, v]
        // makes more sense than `bins` empty buckets.
        let half = 0.5_f64.max(min.abs() * f64::EPSILON);
        vec![min - half, max + half]
    } else {
        (0..=bins)
            .map(|i| min + (span * i as f64 / bins as f64))
            .collect()
    };
    let actual_bins = edges.len() - 1;
    let mut counts = vec![0_usize; actual_bins];
    // Compute `step` once outside the loop — it's a function of
    // min/max/actual_bins, all loop-invariant.
    let step = (max - min) / actual_bins as f64;
    for &v in values {
        // For the equal-width case `(v - min) / step` can produce
        // `actual_bins` for `v == max` due to floating-point; clamp.
        let idx = if v == max {
            actual_bins - 1
        } else {
            ((v - min) / step).floor() as usize
        };
        let bucket = idx.min(actual_bins - 1);
        counts[bucket] += 1;
    }
    Some(Histogram { counts, edges })
}

#[derive(Debug, Clone, PartialEq)]
pub struct Histogram {
    pub counts: Vec<usize>,
    pub edges: Vec<f64>,
}

/// Pearson correlation coefficient. Returns `None` when either
/// series is empty, the two lengths differ, or either series has
/// zero variance (division by zero would produce NaN).
pub fn pearson_correlation(xs: &[f64], ys: &[f64]) -> Option<f64> {
    if xs.is_empty() || xs.len() != ys.len() {
        return None;
    }
    let n = xs.len() as f64;
    let mean_x = xs.iter().sum::<f64>() / n;
    let mean_y = ys.iter().sum::<f64>() / n;
    let mut sxy = 0.0;
    let mut sxx = 0.0;
    let mut syy = 0.0;
    for (&x, &y) in xs.iter().zip(ys.iter()) {
        let dx = x - mean_x;
        let dy = y - mean_y;
        sxy += dx * dy;
        sxx += dx * dx;
        syy += dy * dy;
    }
    let denom = (sxx * syy).sqrt();
    if denom == 0.0 {
        None
    } else {
        Some(sxy / denom)
    }
}

/// Ordinary least-squares simple linear regression `y = slope · x +
/// intercept`. Returns the fit + R² coefficient. Same `None` cases
/// as [`pearson_correlation`].
pub fn linear_regression(xs: &[f64], ys: &[f64]) -> Option<LinearFit> {
    if xs.is_empty() || xs.len() != ys.len() {
        return None;
    }
    let n = xs.len() as f64;
    let mean_x = xs.iter().sum::<f64>() / n;
    let mean_y = ys.iter().sum::<f64>() / n;
    let mut sxy = 0.0;
    let mut sxx = 0.0;
    let mut syy = 0.0;
    for (&x, &y) in xs.iter().zip(ys.iter()) {
        let dx = x - mean_x;
        let dy = y - mean_y;
        sxy += dx * dy;
        sxx += dx * dx;
        syy += dy * dy;
    }
    if sxx == 0.0 {
        return None;
    }
    let slope = sxy / sxx;
    let intercept = mean_y - slope * mean_x;
    // Pearson² = (Σxy)² / (Σx² · Σy²) — same as R² for simple OLS.
    let r_squared = if syy == 0.0 {
        // y is constant: any line through (mean_x, mean_y) fits
        // perfectly; report R² = 1 to convey "no variance to
        // explain", consistent with NumPy.
        1.0
    } else {
        (sxy * sxy) / (sxx * syy)
    };
    Some(LinearFit {
        slope,
        intercept,
        r_squared,
    })
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LinearFit {
    pub slope: f64,
    pub intercept: f64,
    pub r_squared: f64,
}

/// Centered or trailing moving average over `window` points. With
/// `center=false` the result has `n - window + 1` points starting at
/// index `window - 1`. With `center=true` the result has the same
/// length as `values` with NaN padding for the endpoints where the
/// window doesn't fit.
pub fn moving_average(values: &[f64], window: usize, center: bool) -> Option<Vec<f64>> {
    if window == 0 || window > values.len() {
        return None;
    }
    // Rolling sum to avoid recomputing each window from scratch.
    let mut sum = 0.0;
    for i in 0..window {
        sum += values[i];
    }
    let mut means = Vec::with_capacity(values.len() - window + 1);
    means.push(sum / window as f64);
    for i in window..values.len() {
        sum += values[i] - values[i - window];
        means.push(sum / window as f64);
    }
    if !center {
        return Some(means);
    }
    let mut centered = vec![f64::NAN; values.len()];
    let offset = window / 2;
    for (i, m) in means.iter().enumerate() {
        let idx = i + offset;
        if idx < centered.len() {
            centered[idx] = *m;
        }
    }
    Some(centered)
}

/// Sample autocorrelation function up to and including `max_lag`.
/// Returns the value at each lag `0..=max_lag` (lag 0 is always
/// `1.0`). Uses the population variance in the denominator so the
/// outputs are bounded to `[-1, 1]` even for short series.
pub fn autocorrelation(values: &[f64], max_lag: usize) -> Option<Vec<f64>> {
    if values.is_empty() || max_lag >= values.len() {
        return None;
    }
    let n = values.len() as f64;
    let mean = values.iter().sum::<f64>() / n;
    let variance = values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / n;
    if variance == 0.0 {
        return None;
    }
    let mut out = Vec::with_capacity(max_lag + 1);
    for lag in 0..=max_lag {
        let mut cov = 0.0;
        for i in lag..values.len() {
            cov += (values[i] - mean) * (values[i - lag] - mean);
        }
        cov /= n;
        out.push(cov / variance);
    }
    Some(out)
}

/// Classical additive seasonal decomposition. Returns the trend
/// (centered moving average of size `period`), the seasonal
/// component (mean of detrended values at each phase, replicated),
/// and the residual. Trend endpoints (the first and last
/// `period / 2` points) are returned as NaN because the moving
/// average doesn't have enough data to fill them; the seasonal +
/// residual for those points are still produced (using the per-
/// phase seasonal mean) so the lengths all match the input.
///
/// `period` must be at least 2 and at most `values.len() / 2` —
/// otherwise the trend smooth has no signal to recover.
pub fn decompose_seasonal_additive(values: &[f64], period: usize) -> Option<SeasonalDecomposition> {
    if period < 2 || period > values.len() / 2 {
        return None;
    }
    let trend = moving_average(values, period, true)?;
    // De-trend with NaN-aware subtraction; we still need a per-
    // phase mean across all points, so use the trend value where
    // available and the global mean otherwise.
    let global_mean = values.iter().sum::<f64>() / values.len() as f64;
    let detrended: Vec<f64> = values
        .iter()
        .zip(trend.iter())
        .map(|(&v, &t)| if t.is_nan() { v - global_mean } else { v - t })
        .collect();
    let mut seasonal_phase = vec![0.0_f64; period];
    let mut counts = vec![0_usize; period];
    for (i, &d) in detrended.iter().enumerate() {
        let phase = i % period;
        seasonal_phase[phase] += d;
        counts[phase] += 1;
    }
    for phase in 0..period {
        if counts[phase] > 0 {
            seasonal_phase[phase] /= counts[phase] as f64;
        }
    }
    // Centre the seasonal component so its mean is zero (the
    // textbook convention; trend absorbs the bias).
    let seasonal_mean = seasonal_phase.iter().sum::<f64>() / period as f64;
    for s in seasonal_phase.iter_mut() {
        *s -= seasonal_mean;
    }
    let seasonal: Vec<f64> = (0..values.len())
        .map(|i| seasonal_phase[i % period])
        .collect();
    let residual: Vec<f64> = values
        .iter()
        .zip(trend.iter())
        .zip(seasonal.iter())
        .map(|((&v, &t), &s)| if t.is_nan() { f64::NAN } else { v - t - s })
        .collect();
    Some(SeasonalDecomposition {
        trend,
        seasonal,
        residual,
        period,
    })
}

#[derive(Debug, Clone, PartialEq)]
pub struct SeasonalDecomposition {
    pub trend: Vec<f64>,
    pub seasonal: Vec<f64>,
    pub residual: Vec<f64>,
    pub period: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx_eq(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn summary_basic() {
        let s = summary(&[1.0, 2.0, 3.0, 4.0, 5.0]).unwrap();
        assert_eq!(s.count, 5);
        assert!(approx_eq(s.mean, 3.0));
        assert!(approx_eq(s.median, 3.0));
        assert!(approx_eq(s.min, 1.0));
        assert!(approx_eq(s.max, 5.0));
        assert!(approx_eq(s.sum, 15.0));
        // Population variance: ((1-3)² + (2-3)² + ... ) / 5 = 10/5 = 2
        assert!(approx_eq(s.variance, 2.0));
    }

    #[test]
    fn summary_even_length_median() {
        let s = summary(&[1.0, 2.0, 3.0, 4.0]).unwrap();
        assert!(approx_eq(s.median, 2.5));
    }

    #[test]
    fn summary_empty_is_none() {
        assert!(summary(&[]).is_none());
    }

    #[test]
    fn percentile_interpolation() {
        let p50 = percentile(&[1.0, 2.0, 3.0, 4.0], 50.0).unwrap();
        assert!(approx_eq(p50, 2.5));
        let p25 = percentile(&[1.0, 2.0, 3.0, 4.0], 25.0).unwrap();
        assert!(approx_eq(p25, 1.75));
        let p100 = percentile(&[1.0, 2.0, 3.0, 4.0], 100.0).unwrap();
        assert!(approx_eq(p100, 4.0));
    }

    #[test]
    fn percentile_out_of_range_is_none() {
        assert!(percentile(&[1.0], -1.0).is_none());
        assert!(percentile(&[1.0], 101.0).is_none());
        assert!(percentile(&[], 50.0).is_none());
    }

    #[test]
    fn histogram_uniform() {
        let h = histogram(&[1.0, 2.0, 3.0, 4.0, 5.0], 5).unwrap();
        assert_eq!(h.counts, vec![1, 1, 1, 1, 1]);
        assert_eq!(h.edges.len(), 6);
    }

    #[test]
    fn histogram_all_equal_returns_single_bin() {
        let h = histogram(&[3.0, 3.0, 3.0], 5).unwrap();
        assert_eq!(h.counts.len(), 1);
        assert_eq!(h.counts[0], 3);
    }

    #[test]
    fn pearson_perfect_positive() {
        let r = pearson_correlation(&[1.0, 2.0, 3.0], &[2.0, 4.0, 6.0]).unwrap();
        assert!(approx_eq(r, 1.0));
    }

    #[test]
    fn pearson_perfect_negative() {
        let r = pearson_correlation(&[1.0, 2.0, 3.0], &[6.0, 4.0, 2.0]).unwrap();
        assert!(approx_eq(r, -1.0));
    }

    #[test]
    fn pearson_constant_y_is_none() {
        assert!(pearson_correlation(&[1.0, 2.0, 3.0], &[5.0, 5.0, 5.0]).is_none());
    }

    #[test]
    fn linreg_basic() {
        let fit = linear_regression(&[1.0, 2.0, 3.0, 4.0], &[2.0, 4.0, 6.0, 8.0]).unwrap();
        assert!(approx_eq(fit.slope, 2.0));
        assert!(approx_eq(fit.intercept, 0.0));
        assert!(approx_eq(fit.r_squared, 1.0));
    }

    #[test]
    fn moving_average_trailing() {
        let m = moving_average(&[1.0, 2.0, 3.0, 4.0, 5.0], 3, false).unwrap();
        assert_eq!(m.len(), 3);
        assert!(approx_eq(m[0], 2.0));
        assert!(approx_eq(m[1], 3.0));
        assert!(approx_eq(m[2], 4.0));
    }

    #[test]
    fn moving_average_centered_has_nan_padding() {
        let m = moving_average(&[1.0, 2.0, 3.0, 4.0, 5.0], 3, true).unwrap();
        assert_eq!(m.len(), 5);
        assert!(m[0].is_nan());
        assert!(approx_eq(m[1], 2.0));
        assert!(approx_eq(m[3], 4.0));
        assert!(m[4].is_nan());
    }

    #[test]
    fn autocorrelation_lag_zero_is_one() {
        let acf = autocorrelation(&[1.0, 2.0, 3.0, 4.0, 5.0], 2).unwrap();
        assert!(approx_eq(acf[0], 1.0));
        assert_eq!(acf.len(), 3);
    }

    #[test]
    fn decompose_seasonal_recovers_signal() {
        // pure period-4 sine wave + linear trend
        let mut data = Vec::new();
        for i in 0..16 {
            let trend_val = i as f64;
            let seasonal_val = (i % 4) as f64 - 1.5;
            data.push(trend_val + seasonal_val);
        }
        let decomp = decompose_seasonal_additive(&data, 4).unwrap();
        // Seasonal component should oscillate; non-NaN trend values
        // should approximate the linear trend.
        assert_eq!(decomp.period, 4);
        let s_mean = decomp.seasonal.iter().sum::<f64>() / decomp.seasonal.len() as f64;
        assert!(
            s_mean.abs() < 0.5,
            "centered seasonal mean ≈ 0; got {s_mean}"
        );
    }
}
