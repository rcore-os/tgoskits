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
}
