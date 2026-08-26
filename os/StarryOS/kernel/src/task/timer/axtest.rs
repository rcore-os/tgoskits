use ax_runtime::hal::time::TimeValue;
use starry_signal::Signo;

use super::{
    ITimerSetting, ITimerType, ProcessCpuTimeSnapshot, ProcessTimerManager, accounting, alarm,
    time_value_from_nanos,
};

fn itimer_type_signo_and_time_conversion_rules_hold_for_test() -> bool {
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

fn interval_timer_active_gate_rules_hold_for_test() -> bool {
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

fn interval_timer_arm_uses_current_snapshot_for_test() -> bool {
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

fn cpu_interval_timers_avoid_wall_alarms_for_test() -> bool {
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

fn process_cpu_high_water_preserves_runtime_total_for_test() -> bool {
    accounting::process_cpu_high_water_preserves_runtime_total_for_test()
}

fn zero_process_cpu_time_delta_avoids_publication_for_test() -> bool {
    accounting::zero_process_cpu_time_delta_avoids_publication_for_test()
}

fn alarm_generation_rules_hold_for_test() -> bool {
    alarm::stale_alarm_cancellation_preserves_new_generation_for_test()
}

#[axtest::axtest]
fn itimer_type_signo_and_time_conversion_rules_hold() {
    assert!(itimer_type_signo_and_time_conversion_rules_hold_for_test());
}

#[axtest::axtest]
fn interval_timer_active_gate_rules_hold() {
    assert!(interval_timer_active_gate_rules_hold_for_test());
}

#[axtest::axtest]
fn interval_timer_arm_uses_current_snapshot() {
    assert!(interval_timer_arm_uses_current_snapshot_for_test());
}

#[axtest::axtest]
fn cpu_interval_timers_avoid_wall_alarms() {
    assert!(cpu_interval_timers_avoid_wall_alarms_for_test());
}

#[axtest::axtest]
fn process_cpu_high_water_preserves_runtime_total() {
    assert!(process_cpu_high_water_preserves_runtime_total_for_test());
}

#[axtest::axtest]
fn zero_process_cpu_time_delta_avoids_publication() {
    assert!(zero_process_cpu_time_delta_avoids_publication_for_test());
}

#[axtest::axtest]
fn alarm_generation_rules_hold() {
    assert!(alarm_generation_rules_hold_for_test());
}
