//! Small, platform-independent helpers for the soak test's one-off
//! measurements (Settings page rendering, resize handling). Only the soak
//! test uses them; normal application code never does.

/// Summary of a set of per-operation timings, in milliseconds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Summary {
    pub n: usize,
    pub mean: f64,
    pub p50: f64,
    pub p95: f64,
    pub max: f64,
}

impl Summary {
    /// Summarise `samples` (sorted in place). `None` for an empty set or
    /// if any sample is not finite.
    pub fn of(samples: &mut [f64]) -> Option<Summary> {
        if samples.is_empty() || samples.iter().any(|v| !v.is_finite()) {
            return None;
        }
        samples.sort_by(|a, b| a.total_cmp(b));
        let n = samples.len();
        // Nearest-rank percentile: the smallest value with at least p % of
        // the samples at or below it.
        let rank = |p: usize| samples[((n * p).div_ceil(100)).clamp(1, n) - 1];
        Some(Summary {
            n,
            mean: samples.iter().sum::<f64>() / n as f64,
            p50: rank(50),
            p95: rank(95),
            max: samples[n - 1],
        })
    }

    /// "mean 1.234, p50 1.200, p95 2.000, max 3.000 ms (n=40)".
    pub fn line(&self) -> String {
        format!(
            "mean {:.3}, p50 {:.3}, p95 {:.3}, max {:.3} ms (n={})",
            self.mean, self.p50, self.p95, self.max, self.n
        )
    }
}

/// Average of a counter delta over `n` operations, in milliseconds when the
/// counter is in nanoseconds. Saturates instead of underflowing if a
/// counter was reset in between.
pub fn per_op_ms(before_ns: u64, after_ns: u64, n: usize) -> f64 {
    if n == 0 {
        return 0.0;
    }
    after_ns.saturating_sub(before_ns) as f64 / 1e6 / n as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_of_known_values() {
        let mut v: Vec<f64> = (1..=100).map(f64::from).collect();
        v.reverse(); // order must not matter
        let s = Summary::of(&mut v).unwrap();
        assert_eq!(s.n, 100);
        assert!((s.mean - 50.5).abs() < 1e-9);
        assert_eq!(s.p50, 50.0);
        assert_eq!(s.p95, 95.0);
        assert_eq!(s.max, 100.0);
        assert_eq!(
            s.line(),
            "mean 50.500, p50 50.000, p95 95.000, max 100.000 ms (n=100)"
        );
    }

    #[test]
    fn summary_of_small_sets() {
        let s = Summary::of(&mut [2.5]).unwrap();
        assert_eq!((s.p50, s.p95, s.max, s.mean), (2.5, 2.5, 2.5, 2.5));
        let s = Summary::of(&mut [3.0, 1.0]).unwrap();
        assert_eq!((s.p50, s.p95, s.max), (1.0, 3.0, 3.0));
    }

    #[test]
    fn summary_rejects_empty_and_non_finite() {
        assert_eq!(Summary::of(&mut []), None);
        assert_eq!(Summary::of(&mut [1.0, f64::NAN]), None);
        assert_eq!(Summary::of(&mut [f64::INFINITY]), None);
    }

    #[test]
    fn per_op_average_and_saturation() {
        assert_eq!(per_op_ms(1_000_000, 5_000_000, 4), 1.0);
        assert_eq!(per_op_ms(5, 1, 3), 0.0, "counter reset saturates");
        assert_eq!(per_op_ms(0, 10, 0), 0.0, "no operations");
    }
}
