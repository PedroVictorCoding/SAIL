//! Linear-pool weights: the simplex point that minimizes the mean CRPS of the pool on
//! walk-forward forecasts whose outcome was known before the origin.
//!
//! Each scored origin contributes the Gram matrix of `F_m(x) − 1{x ≥ y}` over the five
//! components, so the CRPS of any pool is a small quadratic form and the exact minimizer
//! over the simplex can be found by enumerating the active subsets. Origins where a
//! component is missing contribute a renormalized form; when any exist, a short
//! Frank–Wolfe refinement corrects the exact full-mask solution.

use super::ComponentId;
use super::dist::Dist;

pub(crate) const COMPONENTS: [ComponentId; 5] = [
    ComponentId::Gbm,
    ComponentId::Ewma,
    ComponentId::Historical,
    ComponentId::Chronos,
    ComponentId::Gbdt,
];

pub(crate) const MIN_ORIGINS: usize = 20;

const GRID_POINTS: usize = 160;
const REFINE_ITERATIONS: usize = 30;

#[derive(Clone, Debug)]
pub(crate) struct OriginScore {
    pub(crate) available: [bool; 5],
    pub(crate) gram: [[f64; 5]; 5],
}

/// The Gram matrix `A_mn = ∫ (F_m − H)(F_n − H) dx` for one origin and outcome.
#[allow(clippy::needless_range_loop)]
pub(crate) fn score_origin(dists: &[Option<Dist>; 5], y: f64) -> OriginScore {
    let available = [
        dists[0].is_some(),
        dists[1].is_some(),
        dists[2].is_some(),
        dists[3].is_some(),
        dists[4].is_some(),
    ];
    let mut gram = [[0.0; 5]; 5];
    if !y.is_finite() || available.iter().all(|value| !value) {
        return OriginScore { available, gram };
    }
    let mut low = y;
    let mut high = y;
    for dist in dists.iter().flatten() {
        low = low.min(dist.lower_bound());
        high = high.max(dist.upper_bound());
    }
    if !low.is_finite() || !high.is_finite() || high - low <= 1e-12 {
        return OriginScore { available, gram };
    }
    let step = (high - low) / GRID_POINTS as f64;
    for index in 0..=GRID_POINTS {
        let x = low + step * index as f64;
        let weight = if index == 0 || index == GRID_POINTS {
            0.5
        } else {
            1.0
        };
        let target = if x >= y { 1.0 } else { 0.0 };
        let mut gap = [0.0; 5];
        for component in 0..5 {
            if let Some(dist) = &dists[component] {
                gap[component] = dist.cdf(x) - target;
            }
        }
        for left in 0..5 {
            if !available[left] {
                continue;
            }
            for right in left..5 {
                if !available[right] {
                    continue;
                }
                gram[left][right] += weight * gap[left] * gap[right];
            }
        }
    }
    for left in 0..5 {
        for right in 0..left {
            gram[left][right] = gram[right][left];
        }
    }
    for row in &mut gram {
        for value in row {
            *value *= step;
        }
    }
    OriginScore { available, gram }
}

pub(crate) fn quadratic(gram: &[[f64; 5]; 5], weights: &[f64; 5]) -> f64 {
    let mut value = 0.0;
    for (left, row) in gram.iter().enumerate() {
        for (right, entry) in row.iter().enumerate() {
            value += weights[left] * entry * weights[right];
        }
    }
    value
}

fn equal_weights(active: &[bool; 5]) -> [f64; 5] {
    let count = active.iter().filter(|value| **value).count();
    if count == 0 {
        return [0.0; 5];
    }
    let share = 1.0 / count as f64;
    let mut weights = [0.0; 5];
    for component in 0..5 {
        if active[component] {
            weights[component] = share;
        }
    }
    weights
}

fn solve(mut matrix: [[f64; 5]; 5], mut rhs: [f64; 5], size: usize) -> Option<[f64; 5]> {
    for column in 0..size {
        let mut pivot = column;
        for row in column + 1..size {
            if matrix[row][column].abs() > matrix[pivot][column].abs() {
                pivot = row;
            }
        }
        if matrix[pivot][column].abs() < 1e-14 {
            return None;
        }
        if pivot != column {
            matrix.swap(pivot, column);
            rhs.swap(pivot, column);
        }
        let diagonal = matrix[column][column];
        for row in column + 1..size {
            let factor = matrix[row][column] / diagonal;
            if factor == 0.0 {
                continue;
            }
            let pivot_row = matrix[column];
            for (entry, value) in matrix[row].iter_mut().enumerate().skip(column) {
                *value -= factor * pivot_row[entry];
            }
            rhs[row] -= factor * rhs[column];
        }
    }
    let mut solution = [0.0; 5];
    for row in (0..size).rev() {
        let mut value = rhs[row];
        for column in row + 1..size {
            value -= matrix[row][column] * solution[column];
        }
        solution[row] = value / matrix[row][row];
    }
    Some(solution)
}

/// The exact minimizer of `wᵀ A w` on the simplex over `active`, by enumerating the
/// positive-weight subsets and checking the KKT conditions.
fn exact_quadratic(gram: &[[f64; 5]; 5], active: &[bool; 5]) -> Option<[f64; 5]> {
    let indices: Vec<usize> = (0..5).filter(|index| active[*index]).collect();
    if indices.is_empty() {
        return None;
    }
    if indices.len() == 1 {
        let mut out = [0.0; 5];
        out[indices[0]] = 1.0;
        return Some(out);
    }
    let mut best: Option<([f64; 5], f64)> = None;
    for mask in 1usize..(1usize << indices.len()) {
        let subset: Vec<usize> = indices
            .iter()
            .enumerate()
            .filter(|(bit, _)| mask & (1 << bit) != 0)
            .map(|(_, index)| *index)
            .collect();
        let size = subset.len();
        let mut block = [[0.0; 5]; 5];
        for (row, &left) in subset.iter().enumerate() {
            for (column, &right) in subset.iter().enumerate() {
                block[row][column] = gram[left][right];
            }
        }
        let Some(vector) = solve(block, [1.0; 5], size) else {
            continue;
        };
        let total: f64 = vector[..size].iter().sum();
        if !total.is_finite() || total.abs() < 1e-12 {
            continue;
        }
        let mut weights = [0.0; 5];
        let mut feasible = true;
        for (index, &component) in subset.iter().enumerate() {
            let value = vector[index] / total;
            if !value.is_finite() || value < -1e-9 {
                feasible = false;
                break;
            }
            weights[component] = value.max(0.0);
        }
        if !feasible {
            continue;
        }
        let sum: f64 = weights.iter().sum();
        if sum <= 0.0 {
            continue;
        }
        for value in &mut weights {
            *value /= sum;
        }
        let mut gradient = [0.0; 5];
        for component in 0..5 {
            let mut value = 0.0;
            for other in 0..5 {
                value += gram[component][other] * weights[other];
            }
            gradient[component] = value;
        }
        let reference = gradient[subset[0]];
        let tolerance = 1e-7 * (1.0 + reference.abs());
        let mut stationary = true;
        for &component in &subset {
            if (gradient[component] - reference).abs() > tolerance {
                stationary = false;
                break;
            }
        }
        if !stationary {
            continue;
        }
        for component in 0..5 {
            if active[component]
                && !subset.contains(&component)
                && gradient[component] < reference - tolerance
            {
                stationary = false;
                break;
            }
        }
        if !stationary {
            continue;
        }
        let objective = quadratic(gram, &weights);
        if !objective.is_finite() {
            continue;
        }
        if best.as_ref().is_none_or(|(_, value)| objective < *value) {
            best = Some((weights, objective));
        }
    }
    best.map(|(weights, _)| weights)
}

fn masked_total(available: &[bool; 5], active: &[bool; 5], weights: &[f64; 5]) -> f64 {
    let mut total = 0.0;
    for component in 0..5 {
        if available[component] && active[component] {
            total += weights[component];
        }
    }
    total
}

fn objective(scores: &[OriginScore], active: &[bool; 5], weights: &[f64; 5]) -> f64 {
    if scores.is_empty() {
        return 0.0;
    }
    let mut sum = 0.0;
    for score in scores {
        let total = masked_total(&score.available, active, weights);
        if total <= 1e-12 {
            continue;
        }
        let mut local = 0.0;
        for left in 0..5 {
            if !score.available[left] || !active[left] {
                continue;
            }
            let left_weight = weights[left] / total;
            for right in 0..5 {
                if !score.available[right] || !active[right] {
                    continue;
                }
                local += left_weight * score.gram[left][right] * (weights[right] / total);
            }
        }
        sum += local;
    }
    sum / scores.len() as f64
}

fn gradient(scores: &[OriginScore], active: &[bool; 5], weights: &[f64; 5]) -> [f64; 5] {
    let mut out = [0.0; 5];
    if scores.is_empty() {
        return out;
    }
    for score in scores {
        let total = masked_total(&score.available, active, weights);
        if total <= 1e-12 {
            continue;
        }
        let mut local = 0.0;
        let mut product = [0.0; 5];
        for left in 0..5 {
            if !score.available[left] || !active[left] {
                continue;
            }
            let left_weight = weights[left] / total;
            for right in 0..5 {
                if !score.available[right] || !active[right] {
                    continue;
                }
                let entry = score.gram[left][right] * (weights[right] / total);
                product[left] += entry;
                local += left_weight * entry;
            }
        }
        for component in 0..5 {
            if score.available[component] && active[component] {
                out[component] += 2.0 * (product[component] - local) / total;
            }
        }
    }
    for value in &mut out {
        *value /= scores.len() as f64;
    }
    out
}

fn refine(scores: &[OriginScore], active: &[bool; 5], start: [f64; 5]) -> [f64; 5] {
    let mut weights = start;
    let mut value = objective(scores, active, &weights);
    for _ in 0..REFINE_ITERATIONS {
        let grad = gradient(scores, active, &weights);
        let mut target = usize::MAX;
        let mut best = f64::INFINITY;
        for component in 0..5 {
            if active[component] && grad[component] < best {
                best = grad[component];
                target = component;
            }
        }
        if target == usize::MAX {
            break;
        }
        let mut direction = [0.0; 5];
        for component in 0..5 {
            direction[component] = -weights[component];
        }
        direction[target] += 1.0;
        let slope: f64 = (0..5).map(|index| grad[index] * direction[index]).sum();
        if slope > -1e-12 {
            break;
        }
        let mut step = 1.0;
        let mut improved = false;
        for _ in 0..40 {
            let mut trial = weights;
            for component in 0..5 {
                trial[component] += step * direction[component];
                if trial[component] < 0.0 && trial[component] > -1e-12 {
                    trial[component] = 0.0;
                }
            }
            let trial_value = objective(scores, active, &trial);
            if trial_value.is_finite() && trial_value <= value + 0.25 * step * slope {
                weights = trial;
                value = trial_value;
                improved = true;
                break;
            }
            step *= 0.5;
        }
        if !improved {
            break;
        }
    }
    weights
}

fn normalize(mut weights: [f64; 5], active: &[bool; 5]) -> [f64; 5] {
    let mut total = 0.0;
    for component in 0..5 {
        if !active[component] || !weights[component].is_finite() || weights[component] < 0.0 {
            weights[component] = 0.0;
        }
        total += weights[component];
    }
    if total <= 0.0 || !total.is_finite() {
        return equal_weights(active);
    }
    for value in &mut weights {
        *value /= total;
        if *value < 1e-15 {
            *value = 0.0;
        }
    }
    weights
}

/// Fits the pool weights. `active` is the set available at the forecast origin; only
/// those can receive weight. Returns the weights and the number of scored origins used.
pub(crate) fn fit_weights(
    scores: &[OriginScore],
    active: [bool; 5],
    initial: [f64; 5],
) -> ([f64; 5], u32) {
    let usable = scores
        .iter()
        .filter(|score| (0..5).any(|component| active[component] && score.available[component]))
        .count();
    if usable == 0 {
        return ([0.0; 5], 0);
    }
    if usable < MIN_ORIGINS {
        return (equal_weights(&active), usable as u32);
    }
    let full: Vec<&OriginScore> = scores
        .iter()
        .filter(|score| (0..5).all(|component| !active[component] || score.available[component]))
        .collect();
    let mut gram = [[0.0; 5]; 5];
    for score in &full {
        for (left, row) in gram.iter_mut().enumerate() {
            for (right, value) in row.iter_mut().enumerate() {
                *value += score.gram[left][right];
            }
        }
    }
    if !full.is_empty() {
        for row in &mut gram {
            for value in row {
                *value /= full.len() as f64;
            }
        }
    }
    let start = if full.is_empty() {
        if initial.iter().all(|value| value.is_finite()) && initial.iter().sum::<f64>() > 0.0 {
            normalize(initial, &active)
        } else {
            equal_weights(&active)
        }
    } else {
        exact_quadratic(&gram, &active).unwrap_or_else(|| equal_weights(&active))
    };
    let refined = if full.len() == scores.len() {
        start
    } else {
        refine(scores, &active, start)
    };
    (normalize(refined, &active), usable as u32)
}

/// Mean CRPS of `weights` over one origin's Gram matrix.
pub(crate) fn score_crps(score: &OriginScore, weights: &[f64; 5]) -> f64 {
    quadratic(&score.gram, weights)
}
