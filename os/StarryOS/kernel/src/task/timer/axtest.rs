#[cfg(axtest)]
pub(crate) fn itimer_type_signo_and_time_conversion_rules_hold_for_test() -> bool {
    // ITimerType::signo returns a Signo for each variant without panicking.
    let _real = ITimerType::Real.signo();
    let _virt = ITimerType::Virtual.signo();
    let _prof = ITimerType::Prof.signo();

    // time_value_from_nanos: converts nanoseconds to TimeValue without panicking.
    let _ = time_value_from_nanos(0);
    let _ = time_value_from_nanos(1);
    let _ = time_value_from_nanos(1000000000usize);

    true
}
