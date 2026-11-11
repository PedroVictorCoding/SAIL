//! Component distributions for the outlook: piecewise-linear CDFs through the reported
//! quantiles, normal tails beyond 5% and 95%, mixture quantiles by bisection, and CRPS by
//! direct integration.
//!
//! CRPS is the continuous form of the mean pinball loss over every level; integrating it
//! directly is both exact for the piecewise-linear CDFs and far cheaper than bisecting 99
//! pooled quantiles for every candidate weight vector.

use crate::forecast::stats::{normal_cdf, normal_quantile};

use super::LEVELS;

const Z05: f64 = -1.644_853_626_951_472_2;
const Z16: f64 = -0.994_457_883_209_753;
const Z84: f64 = 0.994_457_883_209_753;
const Z95: f64 = 1.644_853_626_951_472_2;

const SPREAD: f64 = Z84 - Z16;
const MIN_SIGMA: f64 = 1e-12;
const MEAN_POINTS: usize = 400;

/// A component's distribution of the log return at one step.
#[derive(Clone, Debug)]
pub(crate) struct Dist {
    q: [f64; 5],
    sigma: f64,
    mu_low: f64,
    mu_high: f64,
}

impl Dist {
    /// Builds a distribution from the five reported quantiles. Non-finite input and a
    /// non-monotone spread are repaired; `None` means the values were unusable.
    pub(crate) fn new(raw: [f64; 5]) -> Option<Self> {
        if raw.iter().any(|value| !value.is_finite()) {
            return None;
        }
        let mut q = raw;
        for index in 1..q.len() {
            if q[index] < q[index - 1] {
                q[index] = q[index - 1];
            }
        }
        let spread = (q[3] - q[1]) / SPREAD;
        let sigma = if spread.is_finite() && spread > MIN_SIGMA {
            spread
        } else {
            MIN_SIGMA
        };
        let mu_low = q[0] - sigma * Z05;
        let mu_high = q[4] - sigma * Z95;
        Some(Self {
            q,
            sigma,
            mu_low,
            mu_high,
        })
    }

    /// The piecewise-linear CDF, with a normal tail outside the 5–95% quantiles.
    pub(crate) fn cdf(&self, x: f64) -> f64 {
        if x.is_nan() {
            return 0.5;
        }
        if x <= self.q[0] {
            return normal_cdf((x - self.mu_low) / self.sigma).clamp(0.0, LEVELS[0]);
        }
        if x >= self.q[4] {
            return normal_cdf((x - self.mu_high) / self.sigma).clamp(LEVELS[4], 1.0);
        }
        for index in 0..4 {
            let low = self.q[index];
            let high = self.q[index + 1];
            if x >= low && x <= high {
                if high - low <= 0.0 {
                    return LEVELS[index + 1];
                }
                let p =
                    LEVELS[index] + (x - low) / (high - low) * (LEVELS[index + 1] - LEVELS[index]);
                return p.clamp(LEVELS[index], LEVELS[index + 1]);
            }
        }
        0.5
    }

    /// The quantile function: the inverse of [`Dist::cdf`], with normal tails.
    pub(crate) fn quantile(&self, p: f64) -> f64 {
        let p = p.clamp(1e-9, 1.0 - 1e-9);
        if p < LEVELS[0] {
            return self.mu_low + self.sigma * normal_quantile(p);
        }
        if p > LEVELS[4] {
            return self.mu_high + self.sigma * normal_quantile(p);
        }
        for index in 0..4 {
            let low = LEVELS[index];
            let high = LEVELS[index + 1];
            if p >= low && p <= high {
                let width = high - low;
                if width <= 0.0 {
                    return self.q[index + 1];
                }
                let weight = (p - low) / width;
                return self.q[index] + weight * (self.q[index + 1] - self.q[index]);
            }
        }
        self.q[2]
    }

    /// `E[exp(X)]`, integrated over the quantile function.
    pub(crate) fn mean_factor(&self) -> f64 {
        let low = 1e-6;
        let high = 1.0 - 1e-6;
        let step = (high - low) / MEAN_POINTS as f64;
        let mut sum = 0.0;
        for index in 0..=MEAN_POINTS {
            let p = low + step * index as f64;
            let weight = if index == 0 || index == MEAN_POINTS {
                0.5
            } else {
                1.0
            };
            sum += weight * self.quantile(p).exp();
        }
        let value = sum * step;
        if value.is_finite() && value > 0.0 {
            value
        } else {
            self.q[2].exp()
        }
    }

    /// `E[exp(X)]` for a Gaussian component, from its median and 16–84% spread.
    pub(crate) fn normal_mean_factor(&self) -> f64 {
        let sigma = (self.q[3] - self.q[1]) / SPREAD;
        let sigma = if sigma.is_finite() && sigma > 0.0 {
            sigma
        } else {
            0.0
        };
        (self.q[2] + 0.5 * sigma * sigma).exp()
    }

    pub(crate) fn lower_bound(&self) -> f64 {
        self.quantile(1e-6)
    }

    pub(crate) fn upper_bound(&self) -> f64 {
        self.quantile(1.0 - 1e-6)
    }
}

fn normalize(weights: &[f64]) -> f64 {
    weights.iter().filter(|w| w.is_finite()).sum()
}

/// CDF of the linear pool `Σ w_m F_m(x)`.
pub(crate) fn mixture_cdf(dists: &[&Dist], weights: &[f64], x: f64) -> f64 {
    let total = normalize(weights);
    if total <= 0.0 {
        return 0.5;
    }
    let mut value = 0.0;
    for (dist, weight) in dists.iter().zip(weights) {
        if weight.is_finite() && *weight > 0.0 {
            value += weight * dist.cdf(x);
        }
    }
    (value / total).clamp(0.0, 1.0)
}

/// Quantile of the linear pool, by bisection on [`mixture_cdf`].
pub(crate) fn mixture_quantile(dists: &[&Dist], weights: &[f64], p: f64) -> f64 {
    let mut low = f64::INFINITY;
    let mut high = f64::NEG_INFINITY;
    for (dist, weight) in dists.iter().zip(weights) {
        if weight.is_finite() && *weight > 0.0 {
            let value = dist.quantile(p);
            low = low.min(value);
            high = high.max(value);
        }
    }
    if !low.is_finite() || !high.is_finite() {
        return 0.0;
    }
    if high - low <= 1e-15 {
        return low;
    }
    let mut lower = low;
    let mut upper = high;
    let mut floor = mixture_cdf(dists, weights, lower);
    while floor > p {
        let span = (upper - lower).abs().max(1.0);
        lower -= span;
        floor = mixture_cdf(dists, weights, lower);
        if !floor.is_finite() {
            return low;
        }
    }
    if mixture_cdf(dists, weights, upper) < p {
        return high;
    }
    for _ in 0..120 {
        let middle = 0.5 * (lower + upper);
        if mixture_cdf(dists, weights, middle) < p {
            lower = middle;
        } else {
            upper = middle;
        }
        if upper - lower <= 1e-12 * (1.0 + lower.abs() + upper.abs()) {
            break;
        }
    }
    0.5 * (lower + upper)
}
