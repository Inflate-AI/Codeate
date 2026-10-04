//! Aggregates repeated timing samples (N runs per tool per language) into
//! the mean/median/stdev the results table reports, rather than a single
//! noisy sample.

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Aggregate {
    pub mean: f64,
    pub median: f64,
    pub stdev: f64,
}

/// `samples` must be non-empty. Returns `None` for an empty slice rather
/// than panicking or faking a zero -- there is no meaningful aggregate of
/// zero samples.
pub fn aggregate(samples: &[f64]) -> Option<Aggregate> {
    if samples.is_empty() {
        return None;
    }

    let n = samples.len() as f64;
    let mean = samples.iter().sum::<f64>() / n;

    let mut sorted = samples.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).expect("samples must not be NaN"));
    let median = if sorted.len().is_multiple_of(2) {
        let mid = sorted.len() / 2;
        (sorted[mid - 1] + sorted[mid]) / 2.0
    } else {
        sorted[sorted.len() / 2]
    };

    let variance = samples.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / n;
    let stdev = variance.sqrt();

    Some(Aggregate {
        mean,
        median,
        stdev,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_none_for_an_empty_slice() {
        assert_eq!(aggregate(&[]), None);
    }

    #[test]
    fn a_single_sample_has_zero_stdev_and_equals_itself() {
        let agg = aggregate(&[5.0]).expect("non-empty");
        assert_eq!(agg.mean, 5.0);
        assert_eq!(agg.median, 5.0);
        assert_eq!(agg.stdev, 0.0);
    }

    #[test]
    fn computes_mean_median_stdev_for_an_odd_length_sample() {
        let agg = aggregate(&[1.0, 2.0, 3.0]).expect("non-empty");
        assert_eq!(agg.mean, 2.0);
        assert_eq!(agg.median, 2.0);
        let expected_stdev = (2.0_f64 / 3.0).sqrt();
        assert!((agg.stdev - expected_stdev).abs() < 1e-9);
    }

    #[test]
    fn computes_median_for_an_even_length_sample_as_the_midpoint_average() {
        let agg = aggregate(&[1.0, 2.0, 3.0, 4.0]).expect("non-empty");
        assert_eq!(agg.median, 2.5);
    }

    #[test]
    fn median_is_order_independent() {
        let unsorted = aggregate(&[5.0, 1.0, 3.0, 2.0, 4.0]).expect("non-empty");
        let sorted = aggregate(&[1.0, 2.0, 3.0, 4.0, 5.0]).expect("non-empty");
        assert_eq!(unsorted, sorted);
    }
}
