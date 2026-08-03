#[cfg(test)]
mod scheduler_ipi_tests {
    use std::{sync::mpsc, thread, time::Duration};

    use super::*;
    use crate::{TaskSystem, ThreadSpec};

    #[test]
    fn overdue_scheduler_deadline_becomes_sticky_work_instead_of_a_resolution_timer() {
        let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
        let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
        system
            .install_bootstrap_thread(cpu.as_mut(), ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.bring_cpu_online(cpu.as_mut()).unwrap();
        let contender = system
            .create_thread(ThreadSpec::new(SchedulePolicy::default()))
            .unwrap();
        system.make_ready(contender.id()).unwrap();
        system.enqueue(cpu.as_mut(), contender.id(), 0).unwrap();
        let deadline = cpu.remote().fair_balance_deadline_ns();

        assert_eq!(
            cpu.as_mut().next_oneshot_deadline_ns(deadline, 1),
            None,
            "an overdue scheduler event must not be rearmed at timer resolution"
        );
        assert!(
            cpu.remote().needs_reschedule(),
            "the due deadline must remain visible as scheduler work"
        );
    }

    #[test]
    fn load_summary_reader_does_not_wait_for_stalled_writer() {
        let remote = CpuRemote::create(CpuId::new(0));
        remote.set_load_summary_sequence_for_test(1);

        let reader_remote = Arc::clone(&remote);
        let (started_tx, started_rx) = mpsc::channel();
        let (finished_tx, finished_rx) = mpsc::channel();
        let reader = thread::spawn(move || {
            started_tx.send(()).unwrap();
            let summary = reader_remote.try_load_summary();
            finished_tx.send(summary).unwrap();
        });

        started_rx.recv().unwrap();
        let completed_while_writer_stalled =
            finished_rx.recv_timeout(Duration::from_millis(100)).is_ok();

        // Always release the old implementation's unbounded reader so a red
        // result cannot leak a host thread or hang the test process.
        remote.set_load_summary_sequence_for_test(2);
        reader.join().unwrap();

        assert!(
            completed_while_writer_stalled,
            "remote balancing must not spin forever behind a stalled owner writer"
        );
    }

    #[test]
    fn polling_idle_owner_observes_work_without_a_physical_ipi() {
        let remote = CpuRemote::create(CpuId::new(1));
        assert!(remote.mark_online());
        crate::test_runtime::configure_scheduler_ipi(RuntimeStatus::Success, 0);

        assert!(
            remote.prepare_idle_wait(),
            "the empty owner must publish polling before the producer races"
        );
        assert!(remote.kick_scheduler_work());

        assert_eq!(
            crate::test_runtime::scheduler_ipi_send_count(),
            0,
            "a polling owner will observe sticky work in its final recheck"
        );
        assert!(remote.needs_reschedule());
        remote.finish_idle_wait();
    }

    #[test]
    fn inactive_cpu_accepts_owner_delivery_but_rejects_new_placement() {
        let remote = CpuRemote::create(CpuId::new(1));
        assert!(remote.mark_online());
        assert!(remote.try_deactivate());
        assert_eq!(remote.lifecycle_state(), CpuLifecycleState::Inactive);
        assert!(remote.is_online());
        assert!(!remote.accepts_placement());

        assert!(
            remote.begin_publication().is_none(),
            "placement publication must close at the inactive boundary"
        );
        let owner_delivery = remote
            .begin_owner_delivery()
            .expect("in-flight owner control must remain deliverable while inactive");
        assert!(
            !remote.try_begin_draining(),
            "final draining must wait for in-flight owner delivery"
        );
        remote.cancel_deactivation();
        drop(owner_delivery);
        assert_eq!(remote.lifecycle_state(), CpuLifecycleState::Online);

        assert!(remote.try_deactivate());
        assert!(remote.try_begin_draining());
        assert!(remote.begin_owner_delivery().is_none());
        remote.finish_offline();
        assert_eq!(remote.lifecycle_state(), CpuLifecycleState::Offline);
    }
}
