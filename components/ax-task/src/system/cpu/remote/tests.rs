#[cfg(test)]
mod scheduler_ipi_tests {
    use std::{sync::mpsc, thread, time::Duration};

    use super::*;

    #[test]
    fn overdue_scheduler_deadline_becomes_sticky_work_instead_of_a_resolution_timer() {
        let remote = CpuRemote::create(CpuId::new(0));
        assert!(remote.mark_online());
        let cpu = CpuLocal::create(CpuId::new(0), TaskSystemConfig::new(1), Arc::clone(&remote));
        cpu.replace_scheduler_deadline(Some(100));

        assert_eq!(
            cpu.next_oneshot_deadline_ns(100, 1),
            None,
            "an overdue scheduler event must not be rearmed at timer resolution"
        );
        assert_eq!(cpu.scheduler_deadline_ns(), None);
        assert!(
            remote.needs_reschedule(),
            "the consumed deadline must remain visible as scheduler work"
        );
    }

    #[test]
    fn load_summary_reader_does_not_wait_for_stalled_writer() {
        let remote = CpuRemote::create(CpuId::new(0));
        remote.load_summary_sequence.store(1, Ordering::Release);

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
        remote.load_summary_sequence.store(2, Ordering::Release);
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
}
