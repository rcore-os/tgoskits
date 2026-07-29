#[cfg(test)]
mod scheduler_ipi_tests {
    use std::{sync::mpsc, thread, time::Duration};

    use super::*;

    #[test]
    fn overdue_scheduler_deadline_becomes_sticky_work_instead_of_a_resolution_timer() {
        let remote = CpuRemote::create(CpuId::new(0));
        assert!(remote.mark_online());
        let cpu = CpuLocal::create(CpuId::new(0), TaskSystemConfig::new(1), Arc::clone(&remote));
        cpu.arm_deferred_scheduler_deadline(100);

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
    fn stale_coalesced_completion_cannot_clear_a_newer_doorbell_epoch() {
        let remote = CpuRemote::create(CpuId::new(0));
        let old = remote.claim_scheduler_ipi().unwrap();

        // A safe point may consume the old reason before its transport call
        // reports that an older physical delivery covers it. A later producer
        // can then own a new epoch, which the stale completion must not clear.
        remote.acknowledge_scheduler_ipi();
        let new = remote.claim_scheduler_ipi().unwrap();
        remote.finish_scheduler_ipi_send(old, RuntimeStatus::Busy);

        assert_eq!(remote.scheduler_ipi_pending.load(Ordering::Acquire), new.0);
        assert_ne!(new.0 & IPI_CLAIMED, 0);
    }

    #[test]
    fn coalesced_scheduler_ipi_keeps_the_inflight_delivery_claimed() {
        let remote = CpuRemote::create(CpuId::new(0));
        remote.request_scheduler_work();
        let claim = remote.claim_scheduler_ipi().unwrap();

        remote.finish_scheduler_ipi_send(claim, RuntimeStatus::Busy);

        assert_eq!(
            remote.scheduler_ipi_pending.load(Ordering::Acquire),
            claim.0,
            "Busy means an older physical delivery covers this coalesced epoch"
        );
    }
}
