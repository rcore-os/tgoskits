//! Commit under the owning scheduler transaction.

use super::*;

impl<'a> OwnerRqTxn<'a> {
    /// Commits every rq-derived publication exactly once and releases the rq.
    ///
    /// This is the ax-task equivalent of leaving one Linux rq-lock
    /// transaction after `put_prev_task()`/`pick_next_task()`/`set_next_task()`
    /// and updating cpupri/cpudl/overload from that final state. Publication is
    /// explicit rather than a `Drop` fallback so a partial transition cannot
    /// become externally visible by accident.
    pub(crate) fn commit(mut self) {
        if self.context == OwnerRqContext::OfflineBootstrap {
            task_runtime::fatal_invariant(0x5251_100f, self.remote.owner().as_u32() as usize);
        }
        let run_queue = self
            .run_queue
            .as_mut()
            .expect("an unfinished rq transaction must retain its lock");
        self.system
            .publish_run_queue_summary(self.remote, run_queue);
        self.finished = true;
        drop(self.run_queue.take());
    }

    /// Commits owner-local bootstrap state without publishing an offline rq
    /// into the root-domain priority indexes.
    ///
    /// Linux initializes `rq`, `curr`, and `idle` while the CPU is offline;
    /// cpupri/cpudl publication starts only when the rq joins the online root
    /// domain. Keeping the phases separate also prevents `sched_init()` from
    /// entering the runtime IRQ-exit service through nested index locks.
    pub(crate) fn commit_bootstrap(mut self) {
        if self.context != OwnerRqContext::OfflineBootstrap || self.remote.is_online() {
            task_runtime::fatal_invariant(0x5251_1010, self.remote.owner().as_u32() as usize);
        }
        let run_queue = self
            .run_queue
            .as_mut()
            .expect("an unfinished rq transaction must retain its lock");
        let _ = self.remote.publish_run_queue_load_summary(run_queue);
        self.finished = true;
        drop(self.run_queue.take());
    }

    /// Commits the rq state before the final scheduler-work recheck.
    ///
    /// A request published after the decision sets a sticky entry bit for the
    /// next pass. Owner-inbox work that remains after this transaction is
    /// explicitly rearmed after the rq state becomes visible.
    pub(crate) fn commit_and_finish_scheduler_request(mut self) -> SchedulerRequestClaim {
        let claim = self
            .request
            .take()
            .expect("a scheduler rq transaction must merge its decision claim");
        let remote = self.remote;
        let run_queue = self
            .run_queue
            .as_mut()
            .expect("an unfinished rq transaction must retain its lock");
        self.system.publish_run_queue_summary(remote, run_queue);
        self.finished = true;
        drop(self.run_queue.take());
        remote.finish_scheduler_request();
        claim
    }

    /// Publishes the selected rq state but transfers its raw lock to switch
    /// tail instead of releasing it in the outgoing scheduler context.
    ///
    /// Scheduler work is rechecked while rq remains locked. A concurrent
    /// publication leaves its sticky bit set for the next pass, while the
    /// selected switch cannot race the physical handoff.
    pub(crate) fn commit_and_handoff_scheduler_work(mut self) -> RqSwitchBaton {
        if self.context != OwnerRqContext::SchedulerFrame {
            task_runtime::fatal_invariant(0x5251_1011, self.remote.owner().as_u32() as usize);
        }
        let _claim = self
            .request
            .take()
            .expect("a scheduler rq transaction must merge its decision claim");
        let remote = self.remote;
        let run_queue = self
            .run_queue
            .as_mut()
            .expect("an unfinished rq transaction must retain its lock");
        self.system.publish_run_queue_summary(remote, run_queue);
        self.finished = true;
        let guard = self
            .run_queue
            .take()
            .expect("an unfinished rq transaction must retain its lock");
        remote.finish_scheduler_request();
        // SAFETY: scheduler-frame construction guarantees that this guard has
        // no owned irqsave scope. CpuLocal retains both the lock allocation and
        // the outer IRQ-off scheduler baton until switch-tail completion.
        let raw = unsafe { guard.into_raw_baton() };
        RqSwitchBaton {
            owner: remote.owner(),
            _raw: raw,
        }
    }
}
