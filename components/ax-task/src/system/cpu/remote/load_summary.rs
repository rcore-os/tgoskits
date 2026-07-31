use super::*;

const INCOMING_MIGRATION_OVERFLOW_INVARIANT: u32 = 0x4d49_474f;
const INCOMING_MIGRATION_RELEASE_INVARIANT: u32 = 0x4d49_4752;

#[derive(Debug)]
pub(super) struct RemoteLoadState {
    sequence: AtomicU64,
    runnable: AtomicUsize,
    workload: AtomicUsize,
    incoming_migrations: AtomicUsize,
    flags: AtomicU8,
    current_primary: AtomicU64,
    current_sequence: AtomicU64,
    pushable_primary: AtomicU64,
    pushable_sequence: AtomicU64,
    fair_balance_deadline_ns: AtomicU64,
}

impl RemoteLoadState {
    pub(super) const fn new() -> Self {
        Self {
            sequence: AtomicU64::new(0),
            runnable: AtomicUsize::new(0),
            workload: AtomicUsize::new(0),
            incoming_migrations: AtomicUsize::new(0),
            flags: AtomicU8::new(0),
            current_primary: AtomicU64::new(0),
            current_sequence: AtomicU64::new(0),
            pushable_primary: AtomicU64::new(0),
            pushable_sequence: AtomicU64::new(0),
            fair_balance_deadline_ns: AtomicU64::new(u64::MAX),
        }
    }
}

impl CpuRemote {
    pub(crate) fn publish_load_summary(
        &self,
        current_key: Option<SchedulingKey>,
        pushable_key: Option<SchedulingKey>,
        runnable_count: usize,
        workload_count: usize,
        overloaded: bool,
    ) {
        let write_sequence = self.load.sequence.fetch_add(1, Ordering::AcqRel);
        debug_assert_eq!(write_sequence & 1, 0, "load summary has one owner writer");
        self.load.runnable.store(runnable_count, Ordering::Relaxed);
        self.load.workload.store(workload_count, Ordering::Relaxed);
        let mut flags = 0;
        if let Some(key) = current_key {
            flags |= SUMMARY_CURRENT_PRESENT;
            flags |= (key.class_rank() & SUMMARY_CLASS_MASK) << SUMMARY_CURRENT_CLASS_SHIFT;
            self.load
                .current_primary
                .store(key.primary(), Ordering::Relaxed);
            self.load
                .current_sequence
                .store(key.sequence(), Ordering::Relaxed);
        }
        if let Some(key) = pushable_key {
            flags |= SUMMARY_PUSHABLE_PRESENT;
            flags |= (key.class_rank() & SUMMARY_CLASS_MASK) << SUMMARY_PUSHABLE_CLASS_SHIFT;
            self.load
                .pushable_primary
                .store(key.primary(), Ordering::Relaxed);
            self.load
                .pushable_sequence
                .store(key.sequence(), Ordering::Relaxed);
        }
        if overloaded {
            flags |= SUMMARY_OVERLOADED;
        }
        self.load.flags.store(flags, Ordering::Relaxed);
        self.load.sequence.fetch_add(1, Ordering::Release);
    }

    /// Attempts to return a coherent remotely observable scheduling snapshot.
    ///
    /// The owner publishes under a local IRQ guard, but a remote CPU must not
    /// wait indefinitely if that owner is stopped or fails while its sequence
    /// is odd. Callers treat `None` as an unavailable placement candidate and
    /// retry from a later scheduler safe point.
    pub fn try_load_summary(&self) -> Option<CpuLoadSummary> {
        for _ in 0..LOAD_SUMMARY_READ_RETRIES {
            let epoch = self.load.sequence.load(Ordering::Acquire);
            if epoch & 1 != 0 {
                core::hint::spin_loop();
                continue;
            }
            let runnable_count = self.load.runnable.load(Ordering::Relaxed);
            let workload_count = self.load.workload.load(Ordering::Relaxed);
            let flags = self.load.flags.load(Ordering::Relaxed);
            let current_primary = self.load.current_primary.load(Ordering::Relaxed);
            let current_sequence = self.load.current_sequence.load(Ordering::Relaxed);
            let pushable_primary = self.load.pushable_primary.load(Ordering::Relaxed);
            let pushable_sequence = self.load.pushable_sequence.load(Ordering::Relaxed);
            if self.load.sequence.load(Ordering::Acquire) != epoch {
                continue;
            }
            let current_rank = (flags >> SUMMARY_CURRENT_CLASS_SHIFT) & SUMMARY_CLASS_MASK;
            let pushable_rank = (flags >> SUMMARY_PUSHABLE_CLASS_SHIFT) & SUMMARY_CLASS_MASK;
            return Some(CpuLoadSummary {
                epoch,
                runnable_count,
                workload_count,
                current_key: (flags & SUMMARY_CURRENT_PRESENT != 0)
                    .then(|| SchedulingKey::new(current_rank, current_primary, current_sequence)),
                pushable_key: (flags & SUMMARY_PUSHABLE_PRESENT != 0).then(|| {
                    SchedulingKey::new(pushable_rank, pushable_primary, pushable_sequence)
                }),
                pushable_class: (flags & SUMMARY_PUSHABLE_PRESENT != 0)
                    .then(|| SchedulingClass::from_rank(pushable_rank)),
                overloaded: flags & SUMMARY_OVERLOADED != 0,
            });
        }
        None
    }

    /// Attempts to return the remotely observable queued runnable count.
    pub fn try_runnable_summary(&self) -> Option<usize> {
        self.try_load_summary().map(CpuLoadSummary::runnable_count)
    }

    pub(crate) fn try_placement_load(&self) -> Option<usize> {
        self.try_load_summary().map(|summary| {
            summary
                .workload_count()
                .saturating_add(self.load.incoming_migrations.load(Ordering::Acquire))
        })
    }

    pub(super) fn reserve_incoming_migration(&self) {
        if self
            .load
            .incoming_migrations
            .try_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                count.checked_add(1)
            })
            .is_err()
        {
            task_runtime::fatal_invariant(
                INCOMING_MIGRATION_OVERFLOW_INVARIANT,
                self.owner.as_u32() as usize,
            );
        }
    }

    pub(crate) fn complete_incoming_migrations(&self, count: usize) {
        if count == 0 {
            return;
        }
        let previous = self
            .load
            .incoming_migrations
            .fetch_sub(count, Ordering::AcqRel);
        if previous < count {
            task_runtime::fatal_invariant(
                INCOMING_MIGRATION_RELEASE_INVARIANT,
                self.owner.as_u32() as usize,
            );
        }
    }

    pub(crate) fn fair_balance_due(&self, now_ns: u64) -> bool {
        now_ns >= self.load.fair_balance_deadline_ns.load(Ordering::Acquire)
    }

    pub(crate) fn defer_fair_balance(&self, now_ns: u64, interval_ns: u64) {
        self.load
            .fair_balance_deadline_ns
            .store(now_ns.saturating_add(interval_ns.max(1)), Ordering::Release);
    }

    pub(in crate::system::cpu) fn fair_balance_deadline_ns(&self) -> u64 {
        self.load.fair_balance_deadline_ns.load(Ordering::Acquire)
    }

    pub(super) fn reset_fair_balance_for_offline(&self) {
        self.load
            .fair_balance_deadline_ns
            .store(u64::MAX, Ordering::Relaxed);
    }

    #[cfg(test)]
    pub(super) fn set_load_summary_sequence_for_test(&self, sequence: u64) {
        self.load.sequence.store(sequence, Ordering::Release);
    }
}
