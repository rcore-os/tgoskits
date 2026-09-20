use super::*;

#[axtest::axtest]
fn tick_watchdog_rounding_and_shared_threshold_progression() {
    let period = NonZeroU64::new(1_000_000).unwrap();
    assert_eq!(check_realtime_tick_limit(2, period, 1_500, 5_000), RttimeLimitAction::None);
    assert_eq!(check_realtime_tick_limit(3, period, 1_500, 5_000), RttimeLimitAction::Soft);
    // The process soft limit has advanced after the first signal. The hard
    // limit now controls the rounded tick threshold and must win next.
    assert_eq!(check_realtime_tick_limit(5, period, 1_001_500, 5_000), RttimeLimitAction::None);
    assert_eq!(check_realtime_tick_limit(6, period, 1_001_500, 5_000), RttimeLimitAction::Hard);
    assert_eq!(check_realtime_tick_limit(0, period, 0, u64::MAX), RttimeLimitAction::None);
    assert_eq!(check_realtime_tick_limit(1, period, 0, u64::MAX), RttimeLimitAction::Soft);
    assert_eq!(check_realtime_tick_limit(u64::MAX, period, u64::MAX, u64::MAX), RttimeLimitAction::None);
}
