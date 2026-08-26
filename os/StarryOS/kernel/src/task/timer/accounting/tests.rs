#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "smp")]
    #[test]
    fn running_policy_update_waits_for_execution_transition_writer() {
        use std::{
            sync::{mpsc, Arc},
            thread,
            time::Duration as StdDuration,
        };

        let accounting = Arc::new(CpuTimeAccounting::new());
        accounting.set_state_at(TimerState::User, 0);
        accounting.scheduler_switch_in_at(true, 0);

        let execution_writer = accounting.begin_write();
        let (started_tx, started_rx) = mpsc::channel();
        let (completed_tx, completed_rx) = mpsc::channel();
        let policy_accounting = Arc::clone(&accounting);
        let policy_writer = thread::spawn(move || {
            started_tx.send(()).unwrap();
            let delta = policy_accounting.set_realtime_policy_at(false, 10);
            completed_tx.send(delta).unwrap();
        });

        started_rx.recv().unwrap();
        assert_eq!(
            completed_rx.recv_timeout(StdDuration::from_millis(100)),
            Err(mpsc::RecvTimeoutError::Timeout),
            "a remote policy writer must wait for the execution transition"
        );

        drop(execution_writer);
        assert_eq!(
            completed_rx.recv_timeout(StdDuration::from_secs(1)),
            Ok(CpuTimeDelta {
                raw_user_ns: 0,
                raw_system_ns: 0,
                runtime_ns: 10,
            })
        );
        policy_writer.join().unwrap();
    }

    #[test]
    fn preemption_and_yield_preserve_rttime_but_block_resets_it() {
        let accounting = CpuTimeAccounting::new();
        accounting.set_state_at(TimerState::User, 0);
        accounting.scheduler_switch_in_at(true, 0);
        accounting.scheduler_switch_out_at(scheduler::SwitchReason::Preempted, 500_000);
        assert_eq!(
            accounting.snapshot_at(500_000).realtime_continuous_ns,
            500_000
        );

        accounting.scheduler_switch_in_at(true, 500_000);
        accounting.scheduler_switch_out_at(scheduler::SwitchReason::Yield, 1_000_000);
        assert_eq!(
            accounting.snapshot_at(1_000_000).realtime_continuous_ns,
            1_000_000
        );

        accounting.scheduler_switch_in_at(true, 1_000_000);
        accounting.scheduler_switch_out_at(scheduler::SwitchReason::Preempted, 1_500_000);
        assert_eq!(
            accounting.snapshot_at(1_500_000).realtime_continuous_ns,
            1_500_000
        );

        accounting.scheduler_switch_in_at(true, 1_500_000);
        accounting.scheduler_switch_out_at(scheduler::SwitchReason::Blocked, 2_000_000);
        let blocked = accounting.snapshot_at(2_000_000);
        assert_eq!(blocked.realtime_continuous_ns, 0);
        assert_eq!(blocked.realtime_reset_generation, 1);
    }

    #[test]
    fn leaving_rt_policy_resets_continuous_runtime() {
        let accounting = CpuTimeAccounting::new();
        accounting.set_state_at(TimerState::Kernel, 0);
        accounting.scheduler_switch_in_at(true, 0);
        accounting.set_realtime_policy_at(false, 2_000_000);
        let fair = accounting.snapshot_at(3_000_000);
        assert_eq!(fair.realtime_continuous_ns, 0);
        assert_eq!(fair.runtime_ns, 3_000_000);
        assert_eq!(fair.raw_system_ns, 0);

        accounting.set_realtime_policy_at(true, 3_000_000);
        assert_eq!(
            accounting.snapshot_at(3_500_000).realtime_continuous_ns,
            500_000
        );
    }

    #[test]
    fn owner_policy_update_closes_its_sequence_epoch() {
        let accounting = CpuTimeAccounting::new();
        accounting.scheduler_switch_in_at(true, 0);

        accounting.set_realtime_policy_at(false, 1_000_000);

        assert_eq!(accounting.sequence.load(Ordering::Acquire), 4);
        assert_eq!(accounting.snapshot_at(2_000_000).realtime_continuous_ns, 0);
    }

    #[test]
    fn adjusted_cpu_time_matches_linux_monotonic_runtime_contract() {
        let high_water = SpinLock::new(CpuTimeHighWater::ZERO);

        assert_eq!(
            adjust_cpu_time(0, 0, 10, &high_water),
            CpuTimeHighWater {
                user_ns: 10,
                system_ns: 0,
            }
        );
        assert_eq!(
            adjust_cpu_time(10, 10, 20, &high_water),
            CpuTimeHighWater {
                user_ns: 10,
                system_ns: 10,
            }
        );
        assert_eq!(
            adjust_cpu_time(1, 9, 15, &high_water),
            CpuTimeHighWater {
                user_ns: 10,
                system_ns: 10,
            },
            "a stale runtime sample must return the prior high-water mark"
        );
    }
}
