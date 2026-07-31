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
pub(crate) fn cpu_interval_timers_avoid_wall_alarms_for_test() -> bool {
    for (timer, signal) in [
        (ITimerType::Virtual, Signo::SIGVTALRM),
        (ITimerType::Prof, Signo::SIGPROF),
    ] {
        let mut timers = ProcessTimerManager::new();
        let armed_at = ProcessCpuTimeSnapshot {
            user_ns: 100,
            system_ns: 50,
            sampled_at_ns: 1_000,
        };
        let armed = timers.set_itimer(
            timer,
            ITimerSetting::new(TimeValue::ZERO, time_value_from_nanos(10)),
            armed_at,
        );
        if armed.publishes_wall_alarm() {
            return false;
        }

        let wall_only_advanced = ProcessCpuTimeSnapshot {
            sampled_at_ns: 1_000_000,
            ..armed_at
        };
        let pending = timers.poll_cpu(wall_only_advanced);
        if pending.signals().next().is_some()
            || pending.publishes_wall_alarm()
            || timers.get_itimer(timer, wall_only_advanced).1.as_nanos() != 10
        {
            return false;
        }

        let cpu_advanced = match timer {
            ITimerType::Virtual => ProcessCpuTimeSnapshot {
                user_ns: 110,
                ..wall_only_advanced
            },
            ITimerType::Prof => ProcessCpuTimeSnapshot {
                system_ns: 60,
                ..wall_only_advanced
            },
            ITimerType::Real => unreachable!(),
        };
        let expired = timers.poll_cpu(cpu_advanced);
        if expired.signals().collect::<alloc::vec::Vec<_>>() != [signal]
            || expired.publishes_wall_alarm()
        {
            return false;
        }
    }
    true
}

#[cfg(axtest)]
pub(crate) fn scheduler_tick_group_accounting_is_aggregate_for_test() -> bool {
    accounting::scheduler_tick_group_accounting_is_aggregate_for_test()
}

#[cfg(axtest)]
pub(crate) fn scheduler_tick_sampling_avoids_owner_writer_for_test() -> bool {
    accounting::scheduler_tick_sampling_avoids_owner_writer_for_test()
}

#[cfg(axtest)]
pub(crate) fn alarm_generation_rules_hold_for_test() -> bool {
    alarm::stale_alarm_cancellation_preserves_new_generation_for_test()
}
