//! Runqueue-owned runtime accounting and scheduler tick work publication.

use super::*;

impl ThreadCore {
    pub(crate) fn begin_runtime_accounting(&self, now_ns: u64) {
        self.begin_runtime_write();
        self.runtime_accounted_until_ns
            .store(now_ns, Ordering::Relaxed);
        self.runtime_running.store(true, Ordering::Relaxed);
        self.finish_runtime_write();
    }

    pub(crate) fn charge_runtime(&self, runtime_ns: u64, now_ns: u64) {
        self.begin_runtime_write();
        let total = self.charged_runtime_ns.load(Ordering::Relaxed);
        self.charged_runtime_ns
            .store(total.saturating_add(runtime_ns), Ordering::Relaxed);
        self.runtime_accounted_until_ns
            .store(now_ns, Ordering::Relaxed);
        self.finish_runtime_write();
    }

    pub(crate) fn finish_runtime_accounting(&self, now_ns: u64) {
        self.begin_runtime_write();
        self.runtime_accounted_until_ns
            .store(now_ns, Ordering::Relaxed);
        self.runtime_running.store(false, Ordering::Relaxed);
        self.finish_runtime_write();
    }

    pub(crate) fn runtime_snapshot(&self, running_now_ns: Option<u64>) -> ThreadRuntimeSnapshot {
        loop {
            let sequence = self.runtime_sequence.load(Ordering::Acquire);
            if sequence & 1 != 0 {
                core::hint::spin_loop();
                continue;
            }
            let charged = self.charged_runtime_ns.load(Ordering::Relaxed);
            let accounted_until = self.runtime_accounted_until_ns.load(Ordering::Relaxed);
            let running = self.runtime_running.load(Ordering::Relaxed);
            if self.runtime_sequence.load(Ordering::Acquire) == sequence {
                let residual = if running {
                    SchedulerTimestamp::from_nanos(
                        running_now_ns
                            .expect("a running thread snapshot must hold its runqueue clock"),
                    )
                    .since(SchedulerTimestamp::from_nanos(accounted_until))
                } else {
                    0
                };
                return ThreadRuntimeSnapshot {
                    charged_runtime_ns: charged.saturating_add(residual),
                    running,
                };
            }
        }
    }

    fn begin_runtime_write(&self) {
        let sequence = self.runtime_sequence.fetch_add(1, Ordering::AcqRel);
        debug_assert_eq!(sequence & 1, 0, "runtime accounting has multiple writers");
    }

    fn finish_runtime_write(&self) {
        let sequence = self.runtime_sequence.fetch_add(1, Ordering::Release);
        debug_assert_eq!(sequence & 1, 1, "runtime accounting writer lost ownership");
    }

    pub(crate) fn transition_state(&self, next: ThreadState) -> Result<(), TaskError> {
        self.state.transition(next)?;
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
