//! Reap, weak-upgrade, scheduler activity, and inbox-delivery gates.

use super::*;

impl ThreadCore {
    pub(crate) fn try_claim_reap(&self) -> bool {
        self.reap_gate
            .compare_exchange(0, REAP_CLAIMED, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    pub(crate) fn external_lease_count(&self) -> usize {
        self.reap_signal.external_lease_count()
    }

    /// Reserves one owner-inbox delivery while exit publication is still open.
    ///
    /// The count outlives the producer-side activity guard and is transferred
    /// with the intrusive message. Registry resource teardown observes this
    /// count independently from scheduler-internal `Arc` references.
    pub(crate) fn reserve_scheduler_inbox_delivery(&self) -> bool {
        let Some(_activity) = self.try_scheduler_activity() else {
            return false;
        };
        if self.state() == ThreadState::Exited {
            return false;
        }
        // The AcqRel increment publishes the resource-lifetime reservation
        // before the producer's Release inbox publication. Once exit closes
        // `scheduler_activity_gate`, no new delivery count can appear; the
        // reaper's Acquire load may therefore treat an observed zero as stable.
        self.scheduler_inbox_deliveries
            .try_update(Ordering::AcqRel, Ordering::Acquire, |deliveries| {
                deliveries.checked_add(1)
            })
            .expect("scheduler inbox delivery count overflow");
        true
    }

    /// Cancels a delivery reservation that was not accepted by an inbox.
    pub(crate) fn cancel_scheduler_inbox_delivery(&self) {
        self.finish_scheduler_inbox_delivery();
    }

    /// Takes responsibility for one delivery detached from an owner inbox.
    pub(crate) fn accept_scheduler_inbox_delivery(&self) -> ThreadSchedulerInboxDelivery<'_> {
        assert!(
            self.scheduler_inbox_deliveries.load(Ordering::Acquire) != 0,
            "owner consumed an unreserved scheduler inbox delivery"
        );
        ThreadSchedulerInboxDelivery { core: self }
    }

    pub(crate) fn scheduler_inbox_delivery_count(&self) -> usize {
        self.scheduler_inbox_deliveries.load(Ordering::Acquire)
    }

    pub(crate) fn publish_affinity_completion(&self, generation: u64) -> bool {
        self.affinity_completion.publish(generation)
    }

    pub(crate) fn notify_affinity_waiters(&self) {
        self.affinity_completion.notify_waiters();
    }

    /// Enters one owner-side delivery section that must not overlap exit.
    pub(crate) fn try_scheduler_activity(&self) -> Option<ThreadSchedulerActivity<'_>> {
        let preempt = crate::runtime::enter_preempt_guard(
            crate::runtime::PreemptGuardSource::SchedulerActivity,
        );
        let mut observed = self.scheduler_activity_gate.load(Ordering::Acquire);
        loop {
            if observed & SCHEDULER_ACTIVITY_CLOSED != 0 {
                release_scheduler_preempt(preempt);
                return None;
            }
            assert!(
                observed < SCHEDULER_ACTIVITY_MAX_READERS,
                "scheduler activity reader count overflow"
            );
            match self.scheduler_activity_gate.compare_exchange_weak(
                observed,
                observed + 1,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    return Some(ThreadSchedulerActivity {
                        core: self,
                        preempt,
                        _not_send: PhantomData,
                    });
                }
                Err(updated) => observed = updated,
            }
        }
    }

    pub(crate) fn close_owned_scheduler_activity(
        self: &Arc<Self>,
    ) -> Option<OwnedThreadSchedulerExit> {
        let preempt = crate::runtime::enter_preempt_guard(
            crate::runtime::PreemptGuardSource::SchedulerActivity,
        );
        if !self.close_scheduler_activity_gate() {
            release_scheduler_preempt(preempt);
            return None;
        }
        Some(OwnedThreadSchedulerExit {
            core: Arc::clone(self),
            preempt,
            sealed: false,
            _not_send: PhantomData,
        })
    }

    pub(crate) fn cancel_reap_claim(&self) {
        self.reap_gate.store(0, Ordering::Release);
    }

    pub(super) fn finish_scheduler_activity(&self) {
        let previous = self.scheduler_activity_gate.fetch_sub(1, Ordering::Release);
        assert!(
            previous & SCHEDULER_ACTIVITY_MAX_READERS != 0,
            "unbalanced scheduler activity guard"
        );
    }

    fn close_scheduler_activity_gate(&self) -> bool {
        let previous = self
            .scheduler_activity_gate
            .fetch_or(SCHEDULER_ACTIVITY_CLOSED, Ordering::AcqRel);
        if previous & SCHEDULER_ACTIVITY_CLOSED != 0 {
            return false;
        }
        // Activity guards disable task preemption before incrementing the
        // reader count. Hard-IRQ and scheduler-frame callers are already
        // non-preemptible. Waiting here is therefore the same bounded raw-lock
        // handoff as Linux's task pi_lock: no sleeping owner can retain a
        // reader indefinitely, and no new reader can enter after the close bit.
        while self.scheduler_activity_gate.load(Ordering::Acquire) != SCHEDULER_ACTIVITY_CLOSED {
            core::hint::spin_loop();
        }
        true
    }

    pub(super) fn reopen_scheduler_activity(&self) {
        assert_eq!(
            self.scheduler_activity_gate.compare_exchange(
                SCHEDULER_ACTIVITY_CLOSED,
                0,
                Ordering::Release,
                Ordering::Acquire,
            ),
            Ok(SCHEDULER_ACTIVITY_CLOSED),
            "only a quiescent uncommitted exit may reopen scheduler activity"
        );
    }

    pub(super) fn finish_scheduler_inbox_delivery(&self) {
        // AcqRel pairs with the reaper's Acquire count check and also observes
        // an exit state published before the scheduler activity gate reopened.
        // The last delivery republishes task work so a reaper pass that saw a
        // non-zero count cannot become the final, lost retry.
        let previous = self
            .scheduler_inbox_deliveries
            .fetch_sub(1, Ordering::AcqRel);
        assert!(previous != 0, "unbalanced scheduler inbox delivery");
        if previous == 1 && self.reap_signal.exited.load(Ordering::Acquire) {
            self.reap_signal.publish();
        }
    }

    pub(super) fn try_enter_weak_upgrade(&self) -> bool {
        let mut observed = self.reap_gate.load(Ordering::Acquire);
        loop {
            if observed & REAP_CLAIMED != 0 {
                return false;
            }
            assert!(
                observed < REAP_MAX_UPGRADE_READERS,
                "thread weak-upgrade reader count overflow"
            );
            match self.reap_gate.compare_exchange_weak(
                observed,
                observed + 1,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                Ok(_) => return true,
                Err(updated) => observed = updated,
            }
        }
    }

    pub(super) fn exit_weak_upgrade(&self) {
        let previous = self.reap_gate.fetch_sub(1, Ordering::Release);
        assert!(
            previous != 0 && previous & REAP_CLAIMED == 0,
            "unbalanced thread weak-upgrade gate"
        );
    }
}
