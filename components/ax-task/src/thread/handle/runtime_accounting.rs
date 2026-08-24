//! Runqueue-owned runtime accounting and scheduler tick work publication.

use super::*;

impl ThreadCore {
    pub(crate) fn commit_runtime_interval(&self, runtime_ns: u64) {
        if runtime_ns == 0 {
            return;
        }
        self.committed_runtime_ns
            .try_update(Ordering::Release, Ordering::Relaxed, |committed| {
                Some(committed.saturating_add(runtime_ns))
            })
            .expect("runtime commit update always supplies a value");
    }

    #[cfg(all(axtest, feature = "axtest"))]
    pub(crate) fn runtime_committed_ns_for_test(&self) -> u64 {
        self.committed_runtime_ns.load(Ordering::Acquire)
    }

    pub(crate) fn runtime_snapshot(
        &self,
        running_interval_ns: Option<u64>,
    ) -> ThreadRuntimeSnapshot {
        let committed = self.committed_runtime_ns.load(Ordering::Acquire);
        ThreadRuntimeSnapshot {
            charged_runtime_ns: committed.saturating_add(running_interval_ns.unwrap_or_default()),
            running: running_interval_ns.is_some(),
        }
    }

    pub(crate) fn sample_scheduler_tick_cpu_time(&self, tick_ns: u64) {
        if let Some(accounting) = &self.scheduler_tick_cpu_time {
            accounting.sample(tick_ns);
        }
    }

    pub(crate) fn transition_state(&self, next: ThreadState) -> Result<(), TaskError> {
        #[cfg(feature = "task-test-hooks")]
        let previous = self.state.state();
        self.state.transition(next)?;
        #[cfg(feature = "task-test-hooks")]
        crate::task_test_hooks::record_runnable_handoff_transition(self.id, previous, next);
        if next == ThreadState::Exited {
            self.reap_signal.mark_exited();
        }
        Ok(())
    }

    pub(crate) fn begin_scheduler_tick_work(&self, observed_ns: u64) -> bool {
        let Some(work) = self.scheduler_tick_work.as_ref() else {
            return false;
        };
        let Some(generation) = work.enabled_generation() else {
            return false;
        };
        self.scheduler_tick_observed_ns
            .fetch_max(observed_ns, Ordering::AcqRel);
        let mut pending = self.scheduler_tick_work_generation.load(Ordering::Acquire);
        loop {
            // Even an already-pending generation must perform an RMW. This
            // publishes the timestamp to the consumer's generation claim. If
            // the consumer raced ahead and cleared the generation, the CAS
            // fails and this producer installs a fresh physical publication.
            match self.scheduler_tick_work_generation.compare_exchange_weak(
                pending,
                generation,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return pending == 0,
                Err(current) => pending = current,
            }
        }
    }

    pub(crate) fn cancel_scheduler_tick_work(&self) {
        assert!(
            self.scheduler_tick_work_generation
                .swap(0, Ordering::AcqRel)
                != 0,
            "scheduler tick work cancellation requires a pending publication"
        );
    }

    pub(crate) fn take_scheduler_tick_work(&self) -> Option<SchedulerTickWorkClaim> {
        let generation = self
            .scheduler_tick_work_generation
            .swap(0, Ordering::AcqRel);
        assert!(
            generation != 0,
            "scheduler tick work consumption requires a pending publication"
        );
        // Keep the timestamp as a monotonic watermark instead of consuming it.
        // A new IRQ may publish after the generation claim but before this
        // load; retaining the watermark lets both the claimed work and any
        // newly queued generation observe a valid timestamp.
        let observed_ns = self.scheduler_tick_observed_ns.load(Ordering::Acquire);
        self.scheduler_tick_work
            .as_ref()
            .filter(|work| work.generation_is_enabled(generation))
            .cloned()
            .map(|work| SchedulerTickWorkClaim::new(work, generation, observed_ns))
    }

    /// Reclaims publication ownership after a transient callback conflict.
    ///
    /// A tick that arrives after [`Self::take_scheduler_tick_work`] may already
    /// have installed the same or a newer generation and published a new
    /// intrusive message. In that case the compare-exchange fails and that
    /// producer owns delivery. A disabled generation is never replayed.
    pub(crate) fn retry_scheduler_tick_work(&self, claim: &SchedulerTickWorkClaim) -> bool {
        if !claim.generation_is_enabled() {
            return false;
        }
        self.scheduler_tick_work_generation
            .compare_exchange(0, claim.generation(), Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }
}
