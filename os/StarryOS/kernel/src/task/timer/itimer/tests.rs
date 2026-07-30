#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timer_poll_returns_a_bounded_signal_batch_without_a_callback() {
        let mut manager = ProcessTimerManager::new();
        for timer in &mut manager.itimers {
            *timer = ITimer {
                interval_ns: 0,
                deadline_ns: Some(5),
            };
        }

        let pending = manager.poll_at(
            ProcessCpuTimeSnapshot {
                user_ns: 10,
                system_ns: 0,
                sampled_at_ns: 10,
            },
            false,
        );
        let signals: alloc::vec::Vec<_> = pending.signals().collect();

        assert_eq!(signals.len(), 3);
        assert!(signals.contains(&Signo::SIGALRM));
        assert!(signals.contains(&Signo::SIGVTALRM));
        assert!(signals.contains(&Signo::SIGPROF));
    }

    #[test]
    fn active_mask_tracks_only_armed_interval_timers() {
        let mut manager = ProcessTimerManager::new();
        let snapshot = snapshot_at(0, 0, 0);
        assert_eq!(manager.active_mask(), 0);

        let _real = manager.set_itimer(ITimerType::Real, setting(0, 10), snapshot);
        assert_eq!(manager.active_mask(), 1 << ITimerType::Real as usize);

        let _virtual = manager.set_itimer(ITimerType::Virtual, setting(5, 20), snapshot);
        assert_eq!(
            manager.active_mask(),
            (1 << ITimerType::Real as usize) | (1 << ITimerType::Virtual as usize)
        );

        let _disarm_real = manager.set_itimer(ITimerType::Real, setting(0, 0), snapshot);
        assert_eq!(
            manager.active_mask(),
            1 << ITimerType::Virtual as usize
        );

        let _cancellation = manager.cancel_alarm();
        assert_eq!(manager.active_mask(), 0);
    }

    #[test]
    fn newly_armed_timer_does_not_consume_prior_clock_time() {
        const SECOND: u64 = 1_000_000_000;

        let mut manager = ProcessTimerManager::new();
        let armed_at = snapshot_at(3 * SECOND, 2 * SECOND, 5 * SECOND);
        let _armed = manager.set_itimer(
            ITimerType::Real,
            setting(0, 2 * SECOND),
            armed_at,
        );

        let pending = manager.poll(armed_at);

        assert_eq!(pending.signals().next(), None);
        assert_eq!(
            manager.get_itimer(ITimerType::Real, armed_at).1,
            time_value_from_nanos(2 * SECOND)
        );
    }

    #[test]
    fn cpu_timers_do_not_publish_wall_clock_alarms() {
        for (timer, signal) in [
            (ITimerType::Virtual, Signo::SIGVTALRM),
            (ITimerType::Prof, Signo::SIGPROF),
        ] {
            let mut manager = ProcessTimerManager::new();
            let armed_at = snapshot_at(100, 50, 1_000);
            let armed = manager.set_itimer(timer, setting(0, 10), armed_at);

            assert!(
                !armed.publishes_wall_alarm(),
                "{timer:?} must be driven by CPU accounting, not wall time"
            );

            let wall_only_advanced = snapshot_at(100, 50, 1_000_000);
            let pending = manager.poll(wall_only_advanced);
            assert_eq!(pending.signals().next(), None);
            assert!(!pending.publishes_wall_alarm());
            assert_eq!(
                manager.get_itimer(timer, wall_only_advanced).1,
                time_value_from_nanos(10)
            );

            let cpu_advanced = match timer {
                ITimerType::Virtual => snapshot_at(110, 50, 1_000_001),
                ITimerType::Prof => snapshot_at(100, 60, 1_000_001),
                ITimerType::Real => unreachable!(),
            };
            let expired = manager.poll(cpu_advanced);
            assert_eq!(expired.signals().collect::<alloc::vec::Vec<_>>(), [signal]);
            assert!(!expired.publishes_wall_alarm());
        }
    }

    #[test]
    fn periodic_timer_coalesces_missed_periods_without_deadline_drift() {
        let mut manager = ProcessTimerManager::new();
        let armed_at = snapshot_at(0, 0, 100);
        let _armed = manager.set_itimer(ITimerType::Real, setting(10, 10), armed_at);

        let late_snapshot = snapshot_at(0, 0, 135);
        let first_poll = manager.poll(late_snapshot);
        let second_poll = manager.poll(late_snapshot);

        assert_eq!(first_poll.signals().collect::<alloc::vec::Vec<_>>(), [Signo::SIGALRM]);
        assert_eq!(second_poll.signals().next(), None);
        assert_eq!(
            manager.get_itimer(ITimerType::Real, late_snapshot).1,
            time_value_from_nanos(5)
        );
    }

    fn setting(interval_ns: u64, remaining_ns: u64) -> ITimerSetting {
        ITimerSetting::new(
            time_value_from_nanos(interval_ns),
            time_value_from_nanos(remaining_ns),
        )
    }

    const fn snapshot_at(
        user_ns: u64,
        system_ns: u64,
        sampled_at_ns: u64,
    ) -> ProcessCpuTimeSnapshot {
        ProcessCpuTimeSnapshot {
            user_ns,
            system_ns,
            sampled_at_ns,
        }
    }
}
