//! Small, dependency-free statistics for the forecast baselines: the normal distribution,
//! sample moments, type-7 quantiles, proper scoring rules and Spearman rank correlation.
//!
//! **Accuracy.** The normal CDF uses Abramowitz–Stegun 7.1.26 (absolute error below
//! `1.5e-7`) and the inverse uses Acklam's rational approximation (relative error below
//! `1.2e-9`). Both are many orders of magnitude finer than what these baselines need.

/// `sqrt(pi)`, used by the closed-form Gaussian CRPS.
const SQRT_PI: f64 = 1.772_453_850_905_516;

/// `sqrt(2*pi)`, used by the Gaussian density.
const SQRT_2PI: f64 = 2.506_628_274_631_000_7;

/// `ln(2*pi)`.
const LN_2PI: f64 = 1.837_877_066_409_345_5;

/// A tiny positive spread that keeps the Gaussian NLL finite for a zero-variance series.
/// This is a numerical detail of the score, never a published forecast.
const NLL_SIGMA_FLOOR: f64 = 1e-9;

/// Error function, Abramowitz–Stegun 7.1.26.
pub(crate) fn erf(x: f64) -> f64 {
    let sign = if x < 0.0 { -1.0 } else { 1.0 };
    let x = x.abs();
    let t = 1.0 / (1.0 + 0.327_591_1 * x);
    let poly = (((((1.061_405_429 * t - 1.453_152_027) * t) + 1.421_413_741) * t - 0.284_496_736)
        * t
        + 0.254_829_592)
        * t;
    sign * (1.0 - poly * (-x * x).exp())
}

/// Standard normal CDF.
pub(crate) fn normal_cdf(x: f64) -> f64 {
    0.5 * (1.0 + erf(x * std::f64::consts::FRAC_1_SQRT_2))
}

/// Standard normal PDF.
pub(crate) fn normal_pdf(x: f64) -> f64 {
    (-0.5 * x * x).exp() / SQRT_2PI
}

/// Standard normal inverse CDF (quantile function), Acklam's approximation.
#[allow(clippy::excessive_precision)]
///
/// Defined on the open interval `(0, 1)`; ends are clamped so the function is total.
pub(crate) fn normal_quantile(p: f64) -> f64 {
    const A: [f64; 6] = [
        -3.969_683_028_665_376e+01,
        2.209_460_984_245_205e+02,
        -2.759_285_104_469_687e+02,
        1.383_577_518_672_690e+02,
        -3.066_479_806_614_716e+01,
        2.506_628_277_459_239e+00,
    ];
    const B: [f64; 5] = [
        -5.447_609_879_822_406e+01,
        1.615_858_368_580_409e+02,
        -1.556_989_798_598_866e+02,
        6.680_131_188_771_972e+01,
        -1.328_068_155_288_572e+01,
    ];
    const C: [f64; 6] = [
        -7.784_894_002_430_293e-03,
        -3.223_964_580_411_365e-01,
        -2.400_758_277_161_838e+00,
        -2.549_732_539_343_734e+00,
        4.374_664_141_464_968e+00,
        2.938_163_982_698_783e+00,
    ];
    const D: [f64; 4] = [
        7.784_695_709_041_462e-03,
        3.224_671_290_700_398e-01,
        2.445_134_137_142_996e+00,
        3.754_408_661_907_416e+00,
    ];
    const P_LOW: f64 = 0.024_25;
    const P_HIGH: f64 = 1.0 - P_LOW;

    let p = p.clamp(1e-12, 1.0 - 1e-12);
    if p < P_LOW {
        let q = (-2.0 * p.ln()).sqrt();
        (((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5])
            / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0)
    } else if p <= P_HIGH {
        let q = p - 0.5;
        let r = q * q;
        (((((A[0] * r + A[1]) * r + A[2]) * r + A[3]) * r + A[4]) * r + A[5]) * q
            / (((((B[0] * r + B[1]) * r + B[2]) * r + B[3]) * r + B[4]) * r + 1.0)
    } else {
        let q = (-2.0 * (1.0 - p).ln()).sqrt();
        -(((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5])
            / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0)
    }
}

/// Mean of a non-empty slice.
pub(crate) fn mean(values: &[f64]) -> f64 {
    values.iter().sum::<f64>() / values.len() as f64
}

/// Sample standard deviation, denominator `n - 1`. Returns 0 for fewer than two values.
pub(crate) fn sample_std(values: &[f64], mean: f64) -> f64 {
    if values.len() < 2 {
        return 0.0;
    }
    let sum_sq: f64 = values.iter().map(|v| (v - mean) * (v - mean)).sum();
    (sum_sq / (values.len() - 1) as f64).max(0.0).sqrt()
}

/// Quantile of an already sorted slice, Hyndman–Fan type 7 (the usual `quantile()`
/// default in R and NumPy's `linear` method). `p` is clamped to `[0, 1]`.
pub(crate) fn quantile_type7(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    if sorted.len() == 1 {
        return sorted[0];
    }
    let p = p.clamp(0.0, 1.0);
    let h = (sorted.len() - 1) as f64 * p;
    let lower = h.floor() as usize;
    let upper = (lower + 1).min(sorted.len() - 1);
    let weight = h - lower as f64;
    sorted[lower] + weight * (sorted[upper] - sorted[lower])
}

/// CRPS of a Gaussian forecast against one outcome, closed form.
///
/// `sigma * (z*(2*Phi(z) - 1) + 2*phi(z) - 1/sqrt(pi))`, with `z = (y - mu)/sigma`.
/// A zero spread degenerates to the absolute error, which is the correct limit.
pub(crate) fn gaussian_crps(mu: f64, sigma: f64, y: f64) -> f64 {
    if !sigma.is_finite() || sigma <= 0.0 {
        return (y - mu).abs();
    }
    let z = (y - mu) / sigma;
    sigma * (z * (2.0 * normal_cdf(z) - 1.0) + 2.0 * normal_pdf(z) - 1.0 / SQRT_PI)
}

/// CRPS of an empirical forecast: `E|X - y| - 1/2 E|X - X'|` over the sample.
///
/// The second term is evaluated exactly on the sorted sample in `O(n)`.
pub(crate) fn empirical_crps(sorted: &[f64], y: f64) -> f64 {
    let n = sorted.len() as f64;
    if sorted.is_empty() {
        return f64::NAN;
    }
    let first = sorted.iter().map(|x| (x - y).abs()).sum::<f64>() / n;
    let mut pair_sum = 0.0;
    for (i, x) in sorted.iter().enumerate() {
        let coefficient = 2.0 * i as f64 - n + 1.0;
        pair_sum += coefficient * x;
    }
    let spread = 2.0 * pair_sum / (n * n);
    first - 0.5 * spread
}

/// Pinball (quantile) loss for one level.
pub(crate) fn pinball_loss(p: f64, quantile: f64, y: f64) -> f64 {
    let diff = y - quantile;
    if diff >= 0.0 {
        p * diff
    } else {
        (p - 1.0) * diff
    }
}

/// Gaussian negative log-likelihood of one outcome.
pub(crate) fn gaussian_nll(mu: f64, sigma: f64, y: f64) -> f64 {
    let sigma = sigma.max(NLL_SIGMA_FLOOR);
    let z = (y - mu) / sigma;
    0.5 * (z * z + LN_2PI + 2.0 * sigma.ln())
}

/// Average ranks with ties shared, used by the Spearman correlation.
fn average_ranks(values: &[f64]) -> Vec<f64> {
    let mut order: Vec<usize> = (0..values.len()).collect();
    order.sort_by(|a, b| values[*a].total_cmp(&values[*b]));
    let mut ranks = vec![0.0; values.len()];
    let mut start = 0;
    while start < order.len() {
        let mut end = start + 1;
        while end < order.len() && values[order[end]] == values[order[start]] {
            end += 1;
        }
        let rank = (start + 1 + end) as f64 / 2.0;
        for &index in &order[start..end] {
            ranks[index] = rank;
        }
        start = end;
    }
    ranks
}

/// Spearman rank correlation. Returns `None` when there are fewer than two pairs or one
/// side has no rank variation.
pub(crate) fn spearman(pairs: &[(f64, f64)]) -> Option<f64> {
    if pairs.len() < 2 {
        return None;
    }
    let left: Vec<f64> = pairs.iter().map(|(a, _)| *a).collect();
    let right: Vec<f64> = pairs.iter().map(|(_, b)| *b).collect();
    let left_ranks = average_ranks(&left);
    let right_ranks = average_ranks(&right);
    let left_mean = mean(&left_ranks);
    let right_mean = mean(&right_ranks);
    let mut covariance = 0.0;
    let mut left_var = 0.0;
    let mut right_var = 0.0;
    for (l, r) in left_ranks.iter().zip(right_ranks.iter()) {
        let dl = l - left_mean;
        let dr = r - right_mean;
        covariance += dl * dr;
        left_var += dl * dl;
        right_var += dr * dr;
    }
    if left_var <= 0.0 || right_var <= 0.0 {
        return None;
    }
    Some(covariance / (left_var * right_var).sqrt())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normal_quantiles_are_standard() {
        assert!((normal_quantile(0.5)).abs() < 1e-12);
        assert!((normal_quantile(0.975) - 1.959_963_984_540_054).abs() < 1e-8);
        assert!((normal_quantile(0.05) + 1.644_853_626_951_472).abs() < 1e-8);
    }

    #[test]
    fn normal_cdf_matches_known_values() {
        assert!((normal_cdf(0.0) - 0.5).abs() < 1e-7);
        assert!((normal_cdf(1.96) - 0.975_002_104_851_784).abs() < 1e-6);
        assert!((normal_cdf(-1.96) + normal_cdf(1.96) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn type7_quantile_interpolates() {
        let sorted = [1.0, 2.0, 3.0, 4.0];
        assert!((quantile_type7(&sorted, 0.5) - 2.5).abs() < 1e-12);
        assert!((quantile_type7(&sorted, 0.0) - 1.0).abs() < 1e-12);
        assert!((quantile_type7(&sorted, 1.0) - 4.0).abs() < 1e-12);
        assert!((quantile_type7(&sorted, 0.25) - 1.75).abs() < 1e-12);
    }

    #[test]
    fn empirical_crps_matches_brute_force() {
        let sorted: [f64; 3] = [0.0, 1.0, 3.0];
        let y = 2.0;
        let first = (2.0f64 + 1.0 + 1.0) / 3.0;
        let mut spread = 0.0;
        for a in sorted {
            for b in sorted {
                spread += (a - b).abs();
            }
        }
        let expected = first - 0.5 * spread / 9.0;
        assert!((empirical_crps(&sorted, y) - expected).abs() < 1e-12);
    }

    #[test]
    fn spearman_is_one_for_a_monotone_pair() {
        let pairs = [(1.0, 10.0), (2.0, 20.0), (3.0, 45.0), (4.0, 47.0)];
        assert!((spearman(&pairs).unwrap() - 1.0).abs() < 1e-12);
        let inverted = [(1.0, 40.0), (2.0, 30.0), (3.0, 20.0), (4.0, 10.0)];
        assert!((spearman(&inverted).unwrap() + 1.0).abs() < 1e-12);
    }
}
