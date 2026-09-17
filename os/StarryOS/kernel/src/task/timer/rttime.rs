use core::num::NonZeroU64;

/// Checks Linux's RT watchdog tick threshold before deferred signal delivery.
pub(crate) fn check_realtime_tick_limit(
    ticks: u64,
    period_ns: NonZeroU64,
    soft_limit_us: u64,
    hard_limit_us: u64,
) -> RttimeLimitAction {
    if soft_limit_us == u64::MAX {
        return RttimeLimitAction::None;
    }
    let period_ns = u128::from(period_ns.get());
    let threshold_ns = u128::from(soft_limit_us.min(hard_limit_us)) * 1_000;
    // task_tick_rt() only arms the deferred check after timeout exceeds the
    // rounded-up limit. Missed physical periods do not synthesize CPU ticks.
    if u128::from(ticks) <= threshold_ns.div_ceil(period_ns) {
        return RttimeLimitAction::None;
    }
    let runtime_us = u128::from(ticks) * period_ns / 1_000;
    if hard_limit_us != u64::MAX && runtime_us >= u128::from(hard_limit_us) {
        RttimeLimitAction::Hard
    } else if runtime_us >= u128::from(soft_limit_us) {
        RttimeLimitAction::Soft
    } else {
        RttimeLimitAction::None
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RttimeLimitAction {
    None,
    Soft,
    Hard,
}

#[cfg(all(test, axtest))]
mod tests;
