#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timer_poll_returns_a_bounded_signal_batch_without_a_callback() {
        let mut manager = ProcessTimerManager::new();
        for timer in &mut manager.itimers {
            *timer = ITimer {
                interval_ns: 0,
                remained_ns: 5,
                alarm_slot: AlarmSlot::new(),
            };
        }

        let pending = manager.poll_at(
            ProcessCpuTimeSnapshot {
                user_ns: 10,
                system_ns: 0,
                sampled_at_ns: 10,
            },
            None,
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
        assert_eq!(manager.active_mask(), 0);

        let _real = manager.set_itimer(ITimerType::Real, 0, 10);
        assert_eq!(manager.active_mask(), 1 << ITimerType::Real as usize);

        let _virtual = manager.set_itimer(ITimerType::Virtual, 5, 20);
        assert_eq!(
            manager.active_mask(),
            (1 << ITimerType::Real as usize) | (1 << ITimerType::Virtual as usize)
        );

        let _disarm_real = manager.set_itimer(ITimerType::Real, 0, 0);
        assert_eq!(
            manager.active_mask(),
            1 << ITimerType::Virtual as usize
        );

        let _cancellations = manager.cancel_alarms();
        assert_eq!(manager.active_mask(), 0);
    }
}
