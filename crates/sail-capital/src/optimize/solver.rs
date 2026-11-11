//! A small, deterministic convex solver for [`crate::optimize`].
//!
//! The feasible set is the intersection of three convex sets in the tradable weights:
//!
//! - a box with a band on the total: `0 ≤ w_i ≤ upper_i` and `sum_min ≤ Σ w_i ≤ sum_max`
//!   (the cash limits become that band once the locked weights are subtracted);
//! - a half-space capping the high-volatility instruments together;
//! - a ball of limited absolute turnover around the current weights.
//!
//! Projections onto each set are closed and cheap. `project` combines them with
//! **Dykstra's algorithm**, which converges to the projection onto their intersection.
//! Policy mode is exactly that projection of the current weights. Ideas mode minimizes the
//! smooth return/risk trade-off with accelerated projected gradient steps, taking the
//! trading cost as a soft-threshold between the sets.
//!
//! Everything is deterministic: fixed iteration counts, fixed order, no randomness.

/// Dykstra passes. Enough for the small problems this crate solves; the loop also stops
/// early when the point stops moving.
const PROJECTION_PASSES: usize = 600;

/// Accelerated gradient steps for ideas mode.
const GRADIENT_STEPS: usize = 2_000;

/// Iterations of power iteration for the largest covariance eigenvalue.
const POWER_ITERATIONS: usize = 200;

/// The convex constraints one problem imposes on the tradable weights.
pub(crate) struct FeasibleSet<'a> {
    /// Upper bound per tradable instrument.
    pub upper: &'a [f64],
    /// Which tradable instruments count as high-volatility.
    pub high_vol: &'a [bool],
    /// Least total tradable weight (from `cash_max`).
    pub sum_min: f64,
    /// Most total tradable weight (from `cash_min`).
    pub sum_max: f64,
    /// Most total weight among the high-volatility tradables.
    pub high_cap: f64,
    /// Current tradable weights, the turnover centre.
    pub center: &'a [f64],
    /// Largest total absolute change allowed, `2 · max_turnover`.
    pub turnover_radius: f64,
}

impl FeasibleSet<'_> {
    /// Nearest point in the intersection, by Dykstra's alternating projections.
    pub(crate) fn project(&self, point: &[f64]) -> Vec<f64> {
        let count = point.len();
        if count == 0 {
            return Vec::new();
        }
        let mut x = point.to_vec();
        let mut correction_box = vec![0.0; count];
        let mut correction_high = vec![0.0; count];
        let mut correction_turnover = vec![0.0; count];
        let mut buffer = vec![0.0; count];
        let mut previous = x.clone();
        for _ in 0..PROJECTION_PASSES {
            for (index, value) in buffer.iter_mut().enumerate() {
                *value = x[index] + correction_box[index];
            }
            let projected = project_box_sum(&buffer, self.upper, self.sum_min, self.sum_max);
            for index in 0..count {
                correction_box[index] = buffer[index] - projected[index];
                x[index] = projected[index];
            }

            for (index, value) in buffer.iter_mut().enumerate() {
                *value = x[index] + correction_high[index];
            }
            let projected = project_high_vol(&buffer, self.high_vol, self.high_cap);
            for index in 0..count {
                correction_high[index] = buffer[index] - projected[index];
                x[index] = projected[index];
            }

            for (index, value) in buffer.iter_mut().enumerate() {
                *value = x[index] + correction_turnover[index];
            }
            let projected = project_l1_ball(&buffer, self.center, self.turnover_radius);
            for index in 0..count {
                correction_turnover[index] = buffer[index] - projected[index];
                x[index] = projected[index];
            }

            let mut movement: f64 = 0.0;
            for (index, value) in x.iter().enumerate() {
                movement = movement.max((value - previous[index]).abs());
            }
            previous.copy_from_slice(&x);
            if movement < 1e-12 {
                break;
            }
        }
        x
    }

    /// True when the point meets every constraint within `tolerance`.
    pub(crate) fn holds(&self, point: &[f64], tolerance: f64) -> bool {
        if point.len() != self.upper.len() {
            return false;
        }
        let mut total = 0.0;
        let mut high = 0.0;
        let mut turnover = 0.0;
        for (index, value) in point.iter().enumerate() {
            if !value.is_finite() || *value < -tolerance || *value > self.upper[index] + tolerance {
                return false;
            }
            total += value;
            if self.high_vol[index] {
                high += value;
            }
            turnover += (value - self.center[index]).abs();
        }
        total >= self.sum_min - tolerance
            && total <= self.sum_max + tolerance
            && high <= self.high_cap + tolerance
            && turnover <= self.turnover_radius + tolerance
    }
}

/// The ideas-mode objective: `−α'w + (λ/2)·w'Σw + Σ c_i·|w_i − w0_i|` in tradable
/// weights, with the locked part folded into `cross`.
pub(crate) struct IdeasProblem<'a> {
    /// Constraints.
    pub set: FeasibleSet<'a>,
    /// Current tradable weights.
    pub center: &'a [f64],
    /// `α_j` per tradable instrument.
    pub alpha: &'a [f64],
    /// Covariance of the tradable instruments, row-major.
    pub covariance: &'a [f64],
    /// `Σ_j Σ_ij w0_j` over locked instruments, per tradable instrument.
    pub cross: &'a [f64],
    /// Trade cost per tradable instrument, as a fraction of the traded value.
    pub cost: &'a [f64],
    /// Risk aversion `λ`.
    pub lambda: f64,
}

impl IdeasProblem<'_> {
    /// Minimizes the objective over the feasible set from `start`.
    pub(crate) fn solve(&self, start: &[f64], lipschitz: f64) -> Vec<f64> {
        let count = self.center.len();
        if count == 0 {
            return Vec::new();
        }
        let step = 1.0 / lipschitz.max(1e-9);
        let mut current = start.to_vec();
        let mut point = start.to_vec();
        let mut momentum: f64 = 1.0;
        let mut best_value = self.value(&point);
        let mut best = point.clone();
        let mut previous = point.clone();
        for _ in 0..GRADIENT_STEPS {
            let gradient = self.gradient(&point);
            let mut candidate = vec![0.0; count];
            for index in 0..count {
                let moved = point[index] - step * gradient[index];
                let threshold = self.cost[index] * step;
                candidate[index] =
                    self.center[index] + soft_threshold(moved - self.center[index], threshold);
            }
            let projected = self.set.project(&candidate);
            let value = self.value(&projected);
            if value < best_value {
                best_value = value;
                best.copy_from_slice(&projected);
            }
            let mut movement: f64 = 0.0;
            for (index, value) in projected.iter().enumerate() {
                movement = movement.max((value - previous[index]).abs());
            }
            previous.copy_from_slice(&projected);
            let next_momentum = 0.5 * (1.0 + (1.0 + 4.0 * momentum * momentum).sqrt());
            let weight = (momentum - 1.0) / next_momentum;
            for index in 0..count {
                point[index] = projected[index] + weight * (projected[index] - current[index]);
            }
            current.copy_from_slice(&projected);
            momentum = next_momentum;
            if movement < 1e-13 {
                break;
            }
        }
        best
    }

    fn gradient(&self, point: &[f64]) -> Vec<f64> {
        let count = point.len();
        let mut gradient = vec![0.0; count];
        for (index, value) in gradient.iter_mut().enumerate() {
            let mut row = self.cross[index];
            for (column, weight) in point.iter().enumerate() {
                row += self.covariance[index * count + column] * weight;
            }
            *value = -self.alpha[index] + self.lambda * row;
        }
        gradient
    }

    fn value(&self, point: &[f64]) -> f64 {
        let count = point.len();
        let mut quadratic = 0.0;
        let mut linear = 0.0;
        let mut cost = 0.0;
        for index in 0..count {
            let mut row = 0.0;
            for (column, weight) in point.iter().enumerate() {
                row += self.covariance[index * count + column] * weight;
            }
            quadratic += point[index] * row;
            linear += self.alpha[index] * point[index];
            cost += self.cost[index] * (point[index] - self.center[index]).abs();
        }
        let cross: f64 = point
            .iter()
            .zip(self.cross.iter())
            .map(|(value, term)| value * term)
            .sum();
        -linear + 0.5 * self.lambda * (quadratic + 2.0 * cross) + cost
    }
}

/// Largest eigenvalue of a symmetric row-major matrix, by power iteration.
pub(crate) fn largest_eigenvalue(matrix: &[f64], n: usize) -> f64 {
    if n == 0 || matrix.len() != n * n {
        return 0.0;
    }
    let mut vector = vec![1.0 / (n as f64).sqrt(); n];
    for _ in 0..POWER_ITERATIONS {
        let product = mat_vec(matrix, n, &vector);
        let norm = product
            .iter()
            .map(|value| value * value)
            .sum::<f64>()
            .sqrt();
        if !norm.is_finite() || norm <= 0.0 {
            return 0.0;
        }
        for (index, value) in product.iter().enumerate() {
            vector[index] = value / norm;
        }
    }
    let product = mat_vec(matrix, n, &vector);
    vector
        .iter()
        .zip(product.iter())
        .map(|(left, right)| left * right)
        .sum::<f64>()
        .max(0.0)
}

fn mat_vec(matrix: &[f64], n: usize, vector: &[f64]) -> Vec<f64> {
    let mut result = vec![0.0; n];
    for (row, value) in result.iter_mut().enumerate() {
        let mut sum = 0.0;
        for column in 0..n {
            sum += matrix[row * n + column] * vector[column];
        }
        *value = sum;
    }
    result
}

fn soft_threshold(value: f64, threshold: f64) -> f64 {
    if value > threshold {
        value - threshold
    } else if value < -threshold {
        value + threshold
    } else {
        0.0
    }
}

fn project_box_sum(point: &[f64], upper: &[f64], sum_min: f64, sum_max: f64) -> Vec<f64> {
    let mut projected: Vec<f64> = point
        .iter()
        .zip(upper.iter())
        .map(|(value, cap)| value.clamp(0.0, *cap))
        .collect();
    let total: f64 = projected.iter().sum();
    if total < sum_min {
        let reachable: f64 = upper.iter().sum();
        let target = sum_min.min(reachable);
        let shift = solve_shift(&projected, upper, target, true);
        for (index, value) in projected.iter_mut().enumerate() {
            *value = (*value + shift).clamp(0.0, upper[index]);
        }
    } else if total > sum_max {
        let target = sum_max.max(0.0);
        let shift = solve_shift(&projected, upper, target, false);
        for (index, value) in projected.iter_mut().enumerate() {
            *value = (*value - shift).clamp(0.0, upper[index]);
        }
    }
    projected
}

fn solve_shift(point: &[f64], upper: &[f64], target: f64, upwards: bool) -> f64 {
    let mut low = 0.0;
    let mut high = 1.0;
    for _ in 0..80 {
        if shifted_sum(point, upper, high, upwards) >= target {
            break;
        }
        high *= 2.0;
    }
    for _ in 0..120 {
        let middle = 0.5 * (low + high);
        let value = shifted_sum(point, upper, middle, upwards);
        let raise = if upwards {
            value < target
        } else {
            value > target
        };
        if raise {
            low = middle;
        } else {
            high = middle;
        }
    }
    0.5 * (low + high)
}

fn shifted_sum(point: &[f64], upper: &[f64], shift: f64, upwards: bool) -> f64 {
    point
        .iter()
        .zip(upper.iter())
        .map(|(value, cap)| {
            let moved = if upwards {
                value + shift
            } else {
                value - shift
            };
            moved.clamp(0.0, *cap)
        })
        .sum()
}

fn project_high_vol(point: &[f64], high_vol: &[bool], cap: f64) -> Vec<f64> {
    let total: f64 = point
        .iter()
        .zip(high_vol.iter())
        .filter(|(_, flag)| **flag)
        .map(|(value, _)| *value)
        .sum();
    if total <= cap {
        return point.to_vec();
    }
    let count = high_vol.iter().filter(|flag| **flag).count();
    if count == 0 {
        return point.to_vec();
    }
    let shift = (total - cap) / count as f64;
    point
        .iter()
        .zip(high_vol.iter())
        .map(|(value, flag)| if *flag { value - shift } else { *value })
        .collect()
}

fn project_l1_ball(point: &[f64], center: &[f64], radius: f64) -> Vec<f64> {
    let mut delta: Vec<f64> = point
        .iter()
        .zip(center.iter())
        .map(|(value, middle)| value - middle)
        .collect();
    let norm: f64 = delta.iter().map(|value| value.abs()).sum();
    if !norm.is_finite() || norm <= radius {
        return point.to_vec();
    }
    if radius <= 0.0 {
        return center.to_vec();
    }
    let mut sorted: Vec<f64> = delta.iter().map(|value| value.abs()).collect();
    sorted.sort_by(|left, right| right.total_cmp(left));
    let mut cumulative = 0.0;
    let mut theta = 0.0;
    let mut found = false;
    for (index, value) in sorted.iter().enumerate() {
        cumulative += value;
        let candidate = (cumulative - radius) / (index + 1) as f64;
        if *value - candidate > 0.0 {
            theta = candidate;
            found = true;
        }
    }
    if !found {
        theta = (cumulative - radius) / sorted.len() as f64;
    }
    for change in delta.iter_mut() {
        let magnitude = change.abs() - theta;
        *change = if magnitude > 0.0 {
            magnitude.copysign(*change)
        } else {
            0.0
        };
    }
    center
        .iter()
        .zip(delta.iter())
        .map(|(middle, change)| middle + change)
        .collect()
}
