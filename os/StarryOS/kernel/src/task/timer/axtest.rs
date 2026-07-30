#[cfg(axtest)]
pub(crate) fn itimer_type_signo_and_time_conversion_rules_hold_for_test() -> bool {
    // ITimerType::signo returns a Signo for each variant without panicking.
    let _real = ITimerType::Real.signo();
    let _virt = ITimerType::Virtual.signo();
    let _prof = ITimerType::Prof.signo();

    // time_value_from_nanos: converts nanoseconds to TimeValue without panicking.
    let _ = time_value_from_nanos(0);
    let _ = time_value_from_nanos(1);
    let _ = time_value_from_nanos(1_000_000_000u64);

    true
}

#[cfg(axtest)]
pub(crate) fn interval_timer_active_gate_rules_hold_for_test() -> bool {
    let mut timers = ProcessTimerManager::new();
    let snapshot = ProcessCpuTimeSnapshot {
        user_ns: 0,
        system_ns: 0,
        sampled_at_ns: 0,
    };
    if timers.active_mask() != 0 {
        return false;
    }

    let _armed = timers.set_itimer(
        ITimerType::Virtual,
        ITimerSetting::new(
            time_value_from_nanos(1_000),
            time_value_from_nanos(2_000),
        ),
        snapshot,
    );
    if timers.active_mask() != 1 << ITimerType::Virtual as usize {
        return false;
    }

    let _disarmed = timers.set_itimer(
        ITimerType::Virtual,
        ITimerSetting::new(TimeValue::ZERO, TimeValue::ZERO),
        snapshot,
    );
    timers.active_mask() == 0
}

#[cfg(axtest)]
pub(crate) fn interval_timer_arm_uses_current_snapshot_for_test() -> bool {
    const SECOND: u64 = 1_000_000_000;

    let mut timers = ProcessTimerManager::new();
    let armed_at = ProcessCpuTimeSnapshot {
        user_ns: 3 * SECOND,
        system_ns: 2 * SECOND,
        sampled_at_ns: 5 * SECOND,
    };
    let _armed = timers.set_itimer(
        ITimerType::Real,
        ITimerSetting::new(TimeValue::ZERO, time_value_from_nanos(2 * SECOND)),
        armed_at,
    );
    let pending = timers.poll(armed_at);

    pending.signals().next().is_none()
        && timers.get_itimer(ITimerType::Real, armed_at).1.as_nanos()
            == u128::from(2 * SECOND)
}

#[cfg(axtest)]
pub(crate) fn alarm_generation_rules_hold_for_test() -> bool {
    alarm::stale_alarm_cancellation_preserves_new_generation_for_test()
}
