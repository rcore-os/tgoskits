use core::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BenchStats {
    pub iters: u64,
    pub total: Duration,
}

impl BenchStats {
    pub const fn new(iters: u64, total: Duration) -> Self {
        Self { iters, total }
    }

    pub fn avg_ns(self) -> f64 {
        self.total.as_nanos() as f64 / self.iters as f64
    }

    pub fn total_ms(self) -> f64 {
        self.total.as_nanos() as f64 / 1_000_000.0
    }
}

pub fn estimated_switch_ns(roundtrip: BenchStats) -> f64 {
    roundtrip.avg_ns() / 2.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stats_report_average_nanoseconds() {
        let stats = BenchStats::new(4, Duration::from_nanos(1_000));

        assert_eq!(stats.avg_ns(), 250.0);
    }

    #[test]
    fn stats_report_total_milliseconds() {
        let stats = BenchStats::new(1, Duration::from_nanos(2_500_000));

        assert_eq!(stats.total_ms(), 2.5);
    }

    #[test]
    fn switch_estimate_is_half_of_roundtrip() {
        let stats = BenchStats::new(10, Duration::from_nanos(800));

        assert_eq!(estimated_switch_ns(stats), 40.0);
    }
}
