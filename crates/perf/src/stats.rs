//! Percentiles and spread of a window of measurements.

use serde::{Deserialize, Serialize};

/// Percentiles of a set of values (µs, or whatever the caller measured), by the nearest-rank
/// method: each is a value that actually occurred, the smallest with at least that share of the
/// set at or below it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Percentiles {
    pub count: u32,
    pub p50: i64,
    pub p95: i64,
    pub p99: i64,
    pub max: i64,
}

impl Percentiles {
    /// Every value shifted by `by`: percentiles of `x + c` are the percentiles of `x`, plus `c`.
    pub fn shifted(self, by: i64) -> Self {
        Self { p50: self.p50 + by, p95: self.p95 + by, p99: self.p99 + by, max: self.max + by, ..self }
    }
}

/// Percentiles of `values`, which this sorts. `None` for an empty set.
pub fn percentiles(values: &mut [i64]) -> Option<Percentiles> {
    if values.is_empty() {
        return None;
    }
    values.sort_unstable();
    let n = values.len();
    // Nearest rank: the ceil(p * n)-th smallest value, counting from 1.
    let at = |p: usize| values[(p * n).div_ceil(100).clamp(1, n) - 1];
    Some(Percentiles {
        count: n.min(u32::MAX as usize) as u32,
        p50: at(50),
        p95: at(95),
        p99: at(99),
        max: values[n - 1],
    })
}

/// Mean, standard deviation and largest of a set.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Spread {
    pub count: u32,
    pub mean: f64,
    /// Population standard deviation.
    pub stddev: f64,
    pub max: f64,
}

/// The [`Spread`] of `values`; `None` for an empty set.
pub fn spread(values: &[f64]) -> Option<Spread> {
    if values.is_empty() {
        return None;
    }
    let n = values.len() as f64;
    let mean = values.iter().sum::<f64>() / n;
    let variance = values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / n;
    let max = values.iter().copied().fold(f64::MIN, f64::max);
    Some(Spread { count: values.len().min(u32::MAX as usize) as u32, mean, stddev: variance.sqrt(), max })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_rank_percentiles() {
        let mut values: Vec<i64> = (1..=100).rev().collect();
        let p = percentiles(&mut values).unwrap();
        assert_eq!((p.count, p.p50, p.p95, p.p99, p.max), (100, 50, 95, 99, 100));

        // Ten values: p95 and p99 both round up to the largest.
        let mut values: Vec<i64> = vec![7, 1, 9, 3, 5, 2, 8, 4, 10, 6];
        let p = percentiles(&mut values).unwrap();
        assert_eq!((p.p50, p.p95, p.p99, p.max), (5, 10, 10, 10));
    }

    #[test]
    fn single_and_empty_sets() {
        assert_eq!(percentiles(&mut []), None);
        let p = percentiles(&mut [42]).unwrap();
        assert_eq!((p.count, p.p50, p.p95, p.p99, p.max), (1, 42, 42, 42, 42));
    }

    #[test]
    fn a_rare_spike_shows_in_p99_not_p50() {
        let mut values = vec![1_000i64; 990];
        values.extend([30_000i64; 10]);
        let p = percentiles(&mut values).unwrap();
        assert_eq!((p.p50, p.p95, p.p99, p.max), (1_000, 1_000, 1_000, 30_000));
        let mut values = vec![1_000i64; 980];
        values.extend([30_000i64; 20]);
        assert_eq!(percentiles(&mut values).unwrap().p99, 30_000);
    }

    #[test]
    fn negative_values_and_shifting() {
        let mut values = vec![-5i64, 10, -20, 0];
        let p = percentiles(&mut values).unwrap();
        assert_eq!((p.p50, p.max), (-5, 10));
        assert_eq!(p.shifted(100).p50, 95);
        assert_eq!(p.shifted(100).count, 4);
    }

    #[test]
    fn spread_of_steady_and_jittery_frames() {
        let steady = spread(&[4.0, 4.0, 4.0, 4.0]).unwrap();
        assert_eq!((steady.mean, steady.stddev, steady.max), (4.0, 0.0, 4.0));
        let jittery = spread(&[2.0, 6.0, 2.0, 6.0]).unwrap();
        assert_eq!((jittery.mean, jittery.stddev, jittery.max), (4.0, 2.0, 6.0));
        assert!(spread(&[]).is_none());
    }
}
