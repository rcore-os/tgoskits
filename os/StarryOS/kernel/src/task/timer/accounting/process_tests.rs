#[cfg(test)]
mod process_tests {
    use super::*;

    #[test]
    fn process_cpu_snapshot_combines_running_siblings_without_double_counting() {
        let process = ProcessCpuTimeAccounting::new();
        let first = CpuTimeAccounting::new();
        let second = CpuTimeAccounting::new();

        process.record_transition(|| first.set_state_at(TimerState::User, 0));
        process.record_transition(|| {
            first.scheduler_switch_in_at(false, 0);
            CpuTimeDelta::ZERO
        });
        process.record_transition(|| second.set_state_at(TimerState::Kernel, 0));
        process.record_transition(|| {
            second.scheduler_switch_in_at(false, 0);
            CpuTimeDelta::ZERO
        });

        let mut live = |now| {
            first
                .running_residual_at(now)
                .add(second.running_residual_at(now))
        };
        assert_eq!(
            process.snapshot_at_with_live(10, &mut live),
            ProcessCpuTimeSnapshot {
                user_ns: 10,
                system_ns: 10,
                sampled_at_ns: 10,
            }
        );

        process.record_transition(|| {
            first.scheduler_switch_out_at(scheduler::SwitchReason::Preempted, 10)
        });
        assert_eq!(
            process.snapshot_at_with_live(15, &mut live),
            ProcessCpuTimeSnapshot {
                user_ns: 10,
                system_ns: 15,
                sampled_at_ns: 15,
            }
        );

        process.record_transition(|| {
            second.scheduler_switch_out_at(scheduler::SwitchReason::Blocked, 15)
        });
        assert_eq!(
            process.snapshot_at_with_live(15, &mut live),
            ProcessCpuTimeSnapshot {
                user_ns: 10,
                system_ns: 15,
                sampled_at_ns: 15,
            }
        );
    }

    #[test]
    fn process_cpu_snapshot_does_not_wait_for_a_preempted_transition() {
        use std::{sync::mpsc, thread, time::Duration as StdDuration};

        let process = Arc::new(ProcessCpuTimeAccounting::new());
        let task = Arc::new(CpuTimeAccounting::new());
        process.record_transition(|| task.set_state_at(TimerState::User, 0));
        process.record_transition(|| {
            task.scheduler_switch_in_at(false, 0);
            CpuTimeDelta::ZERO
        });
        let mut live = |now| task.running_residual_at(now);
        assert_eq!(
            process.snapshot_at_with_live(10, &mut live),
            ProcessCpuTimeSnapshot {
                user_ns: 10,
                system_ns: 0,
                sampled_at_ns: 10,
            }
        );

        let (transition_started_tx, transition_started_rx) = mpsc::channel();
        let (resume_transition_tx, resume_transition_rx) = mpsc::channel();
        let writer_process = Arc::clone(&process);
        let writer_task = Arc::clone(&task);
        let writer = thread::spawn(move || {
            writer_process.record_transition(|| {
                let delta =
                    writer_task.scheduler_switch_out_at(scheduler::SwitchReason::Preempted, 10);
                transition_started_tx.send(()).unwrap();
                resume_transition_rx.recv().unwrap();
                delta
            });
        });
        transition_started_rx.recv().unwrap();

        let (snapshot_done_tx, snapshot_done_rx) = mpsc::channel();
        let reader_process = Arc::clone(&process);
        let reader_task = Arc::clone(&task);
        let reader = thread::spawn(move || {
            let snapshot = reader_process
                .snapshot_at_with_live(10, &mut |now| reader_task.running_residual_at(now));
            snapshot_done_tx.send(snapshot).unwrap();
        });
        let snapshot_while_writer_is_preempted =
            snapshot_done_rx.recv_timeout(StdDuration::from_secs(1));

        resume_transition_tx.send(()).unwrap();
        writer.join().unwrap();
        reader.join().unwrap();

        assert_eq!(
            snapshot_while_writer_is_preempted
                .expect("a process CPU-time reader must not spin behind a preempted writer"),
            ProcessCpuTimeSnapshot {
                user_ns: 10,
                system_ns: 0,
                sampled_at_ns: 10,
            },
            "the handoff window must not make process CPU time regress"
        );
        assert_eq!(
            process.snapshot_at_with_live(10, &mut live),
            ProcessCpuTimeSnapshot {
                user_ns: 10,
                system_ns: 0,
                sampled_at_ns: 10,
            }
        );
    }

    #[test]
    fn scheduler_ticks_publish_group_time_without_scanning_live_siblings() {
        let process = ProcessCpuTimeAccounting::new();
        let first = CpuTimeAccounting::new();
        let second = CpuTimeAccounting::new();

        process.record_transition(|| first.set_state_at(TimerState::User, 0));
        process.record_transition(|| {
            first.scheduler_switch_in_at(false, 0);
            CpuTimeDelta::ZERO
        });
        process.record_transition(|| second.set_state_at(TimerState::Kernel, 0));
        process.record_transition(|| {
            second.scheduler_switch_in_at(false, 0);
            CpuTimeDelta::ZERO
        });

        assert_eq!(
            process.snapshot_committed_at(10),
            ProcessCpuTimeSnapshot {
                user_ns: 0,
                system_ns: 0,
                sampled_at_ns: 10,
            },
            "live residuals must not enter the O(1) scheduler-tick snapshot implicitly"
        );

        process.record_transition(|| {
            let writer = first.begin_owner_write();
            first.account_now_at(10);
            drop(writer);
            first.publish_committed_delta()
        });
        process.record_transition(|| {
            let writer = second.begin_owner_write();
            second.account_now_at(10);
            drop(writer);
            second.publish_committed_delta()
        });
        assert_eq!(
            process.snapshot_committed_at(10),
            ProcessCpuTimeSnapshot {
                user_ns: 10,
                system_ns: 10,
                sampled_at_ns: 10,
            }
        );
    }
}
