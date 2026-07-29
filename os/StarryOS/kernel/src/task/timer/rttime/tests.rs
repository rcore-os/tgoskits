#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rttime_watchdog_uses_exact_limits_and_one_second_soft_intervals() {
        let mut watchdog = RttimeWatchdog::new();
        assert_eq!(watchdog.check(9, 0, 10, u64::MAX), RttimeLimitAction::None);
        assert_eq!(watchdog.check(10, 0, 10, u64::MAX), RttimeLimitAction::Soft);
        assert_eq!(
            watchdog.check(1_000_009, 0, 10, u64::MAX),
            RttimeLimitAction::None
        );
        assert_eq!(
            watchdog.check(1_000_010, 0, 10, u64::MAX),
            RttimeLimitAction::Soft
        );

        let mut hard_watchdog = RttimeWatchdog::new();
        assert_eq!(
            hard_watchdog.check(19, 0, u64::MAX, 20),
            RttimeLimitAction::None
        );
        assert_eq!(
            hard_watchdog.check(20, 0, u64::MAX, 20),
            RttimeLimitAction::Hard
        );

        let accounting = CpuTimeAccounting::new();
        let mut watchdog = RttimeWatchdog::new();
        assert_eq!(
            watchdog.check_snapshot(accounting.snapshot_at(0), 0, 0),
            RttimeLimitAction::None
        );
    }

    #[test]
    fn rttime_reset_generation_rearms_the_soft_limit() {
        let mut watchdog = RttimeWatchdog::new();
        assert_eq!(watchdog.check(10, 0, 10, u64::MAX), RttimeLimitAction::Soft);
        assert_eq!(watchdog.check(0, 1, 10, u64::MAX), RttimeLimitAction::None);
        assert_eq!(watchdog.check(10, 1, 10, u64::MAX), RttimeLimitAction::Soft);
    }
}
