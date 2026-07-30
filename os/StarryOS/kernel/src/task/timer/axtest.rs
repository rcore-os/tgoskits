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
    if timers.active_mask() != 0 {
        return false;
    }

    let _armed = timers.set_itimer(ITimerType::Virtual, 1_000, 2_000);
    if timers.active_mask() != 1 << ITimerType::Virtual as usize {
        return false;
    }

    let _disarmed = timers.set_itimer(ITimerType::Virtual, 0, 0);
    timers.active_mask() == 0
}
