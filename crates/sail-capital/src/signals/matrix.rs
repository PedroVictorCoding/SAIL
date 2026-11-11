//! Small dense linear algebra for the signal combination: Pearson correlation and a
//! Gauss–Jordan inverse with a ridge on the diagonal.
//!
//! Matrices are row-major `n × n`. Everything is deterministic: the same numbers give the
//! same result, and the pivoting order only depends on the input.

/// Pearson correlation of paired values. Returns `None` with fewer than two pairs or when
/// one side has no spread.
pub(crate) fn pearson(pairs: &[(f64, f64)]) -> Option<f64> {
    if pairs.len() < 2 {
        return None;
    }
    let count = pairs.len() as f64;
    let mean_left = pairs.iter().map(|(left, _)| *left).sum::<f64>() / count;
    let mean_right = pairs.iter().map(|(_, right)| *right).sum::<f64>() / count;
    let mut covariance = 0.0;
    let mut variance_left = 0.0;
    let mut variance_right = 0.0;
    for (left, right) in pairs {
        let delta_left = left - mean_left;
        let delta_right = right - mean_right;
        covariance += delta_left * delta_right;
        variance_left += delta_left * delta_left;
        variance_right += delta_right * delta_right;
    }
    if variance_left <= 0.0 || variance_right <= 0.0 {
        return None;
    }
    Some((covariance / (variance_left * variance_right).sqrt()).clamp(-1.0, 1.0))
}

/// Inverse of `matrix + ridge·I` (row-major `n × n`) by Gauss–Jordan elimination with
/// partial pivoting. `None` when the matrix stays singular.
pub(crate) fn invert_with_ridge(matrix: &[f64], n: usize, ridge: f64) -> Option<Vec<f64>> {
    if matrix.len() != n * n {
        return None;
    }
    let mut work = matrix.to_vec();
    for index in 0..n {
        work[index * n + index] += ridge;
    }
    let mut inverse = vec![0.0; n * n];
    for index in 0..n {
        inverse[index * n + index] = 1.0;
    }
    for column in 0..n {
        let mut pivot = column;
        let mut best = work[column * n + column].abs();
        for row in (column + 1)..n {
            let candidate = work[row * n + column].abs();
            if candidate > best {
                best = candidate;
                pivot = row;
            }
        }
        if !best.is_finite() || best < 1e-12 {
            return None;
        }
        if pivot != column {
            for offset in 0..n {
                work.swap(column * n + offset, pivot * n + offset);
                inverse.swap(column * n + offset, pivot * n + offset);
            }
        }
        let divisor = work[column * n + column];
        for offset in 0..n {
            work[column * n + offset] /= divisor;
            inverse[column * n + offset] /= divisor;
        }
        for row in 0..n {
            if row == column {
                continue;
            }
            let factor = work[row * n + column];
            if factor == 0.0 {
                continue;
            }
            for offset in 0..n {
                work[row * n + offset] -= factor * work[column * n + offset];
                inverse[row * n + offset] -= factor * inverse[column * n + offset];
            }
        }
    }
    Some(inverse)
}

/// `matrix · vector` for a row-major `n × n` matrix.
pub(crate) fn mat_vec(matrix: &[f64], n: usize, vector: &[f64]) -> Vec<f64> {
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

/// Dot product of two equal-length vectors.
pub(crate) fn dot(left: &[f64], right: &[f64]) -> f64 {
    left.iter().zip(right.iter()).map(|(a, b)| a * b).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pearson_is_one_for_a_line() {
        let pairs = [(1.0, 3.0), (2.0, 5.0), (3.0, 7.0)];
        assert!((pearson(&pairs).unwrap() - 1.0).abs() < 1e-12);
        assert!(pearson(&[(1.0, 1.0), (1.0, 2.0)]).is_none());
    }

    #[test]
    fn inverse_of_a_two_by_two_is_exact() {
        let matrix = [4.0, 7.0, 2.0, 6.0];
        let inverse = invert_with_ridge(&matrix, 2, 0.0).expect("invertible");
        for row in 0..2 {
            for column in 0..2 {
                let mut product = 0.0;
                for inner in 0..2 {
                    product += inverse[row * 2 + inner] * matrix[inner * 2 + column];
                }
                let expected = if row == column { 1.0 } else { 0.0 };
                assert!((product - expected).abs() < 1e-12, "{product}");
            }
        }
    }

    #[test]
    fn a_singular_matrix_needs_the_ridge() {
        let matrix = [1.0, 1.0, 1.0, 1.0];
        assert!(invert_with_ridge(&matrix, 2, 0.0).is_none());
        assert!(invert_with_ridge(&matrix, 2, 0.1).is_some());
    }
}
