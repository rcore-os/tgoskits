use super::*;

const INCOMING_MIGRATION_OVERFLOW_INVARIANT: u32 = 0x4d49_474f;
const INCOMING_MIGRATION_RELEASE_INVARIANT: u32 = 0x4d49_4752;
const RT_WAKE_DONOR_PRIORITY_MASK: u16 = 0x7f;
const RT_WAKE_CURRENT_MIGRATION_CAPABLE: u16 = 1 << 7;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RunQueueLoadPublication {
    queued_count: usize,
    nr_running: usize,
    fair_demand: u64,
    workload_demand: u64,
    current_workload_demand: u64,
    fair_pushable: bool,
    rt_wake_donor: u16,
}

#[derive(Debug)]
pub(super) struct RemoteLoadState {
    sequence: AtomicU64,
    queued: AtomicUsize,
    nr_running: AtomicUsize,
    fair_demand: AtomicU64,
    workload_demand: AtomicU64,
    current_workload_demand: AtomicU64,
    incoming_migration_demand: AtomicU64,
    flags: AtomicU16,
    rt_wake_donor: AtomicU16,
}

impl RemoteLoadState {
    pub(super) const fn new() -> Self {
        Self {
            sequence: AtomicU64::new(0),
            queued: AtomicUsize::new(0),
            nr_running: AtomicUsize::new(0),
            fair_demand: AtomicU64::new(0),
            workload_demand: AtomicU64::new(0),
            current_workload_demand: AtomicU64::new(0),
            incoming_migration_demand: AtomicU64::new(0),
            flags: AtomicU16::new(0),
            rt_wake_donor: AtomicU16::new(0),
        }
    }

    fn matches(&self, publication: RunQueueLoadPublication) -> bool {
        self.queued.load(Ordering::Relaxed) == publication.queued_count
            && self.nr_running.load(Ordering::Relaxed) == publication.nr_running
            && self.fair_demand.load(Ordering::Relaxed) == publication.fair_demand
            && self.workload_demand.load(Ordering::Relaxed) == publication.workload_demand
            && self.current_workload_demand.load(Ordering::Relaxed)
                == publication.current_workload_demand
            && (self.flags.load(Ordering::Relaxed) & SUMMARY_FAIR_PUSHABLE != 0)
                == publication.fair_pushable
            && self.rt_wake_donor.load(Ordering::Relaxed) == publication.rt_wake_donor
    }
}

impl CpuRemote {
    fn publish_load_summary(&self, publication: RunQueueLoadPublication) -> bool {
        // Every writer owns this CPU's rq lock, so the previously completed
        // atomic fields are a stable comparison point. Readers still use the
        // sequence protocol for a coherent multi-field snapshot.
        if self.load.matches(publication) {
            return false;
        }
        let RunQueueLoadPublication {
            queued_count,
            nr_running,
            fair_demand,
            workload_demand,
            current_workload_demand,
            fair_pushable,
            rt_wake_donor,
        } = publication;
        let write_sequence = self.load.sequence.fetch_add(1, Ordering::AcqRel);
        debug_assert_eq!(
            write_sequence & 1,
            0,
            "load summary writers must hold the runqueue lock"
        );
        self.load.queued.store(queued_count, Ordering::Relaxed);
        self.load.nr_running.store(nr_running, Ordering::Relaxed);
        self.load.fair_demand.store(fair_demand, Ordering::Relaxed);
        self.load
            .workload_demand
            .store(workload_demand, Ordering::Relaxed);
        self.load
            .current_workload_demand
            .store(current_workload_demand, Ordering::Relaxed);
        let flags = if fair_pushable {
            SUMMARY_FAIR_PUSHABLE
        } else {
            0
        };
        self.load.flags.store(flags, Ordering::Relaxed);
        self.load
            .rt_wake_donor
            .store(rt_wake_donor, Ordering::Release);
        self.load.sequence.fetch_add(1, Ordering::Release);
        true
    }

    /// Publishes the remotely observable load state while the caller owns this
    /// CPU's runqueue lock.
    ///
    /// Taking the runqueue state by reference keeps queue membership, current
    /// priority, and load publication in one transaction for both owner and
    /// direct remote wake paths. Returns whether the committed load state
    /// changed and therefore advanced the publication sequence.
    pub(crate) fn publish_run_queue_load_summary(&self, run_queue: &CpuRunQueueState) -> bool {
        let current = run_queue.current();
        let current_non_idle = current.is_some_and(|current| {
            self.idle_thread()
                .is_none_or(|idle| current.thread() != idle)
        });
        let queued = run_queue.nr_queued();
        let nr_running = run_queue.nr_running();
        let current_fair_demand = current
            .filter(|_| current_non_idle)
            .map_or(0, CurrentDispatch::fair_demand);
        let current_placement_demand = current
            .filter(|_| current_non_idle)
            .map_or(0, CurrentDispatch::placement_demand);
        let fair_demand = run_queue.fair_demand().saturating_add(current_fair_demand);
        let workload_demand = run_queue
            .placement_demand()
            .saturating_add(current_placement_demand);
        let rt_wake_donor = current
            .filter(|_| current_non_idle)
            .and_then(|current| {
                current.schedule_policy().rt_priority().map(|priority| {
                    u16::from(priority.get())
                        | if current.metadata().affinity.is_migration_capable() {
                            RT_WAKE_CURRENT_MIGRATION_CAPABLE
                        } else {
                            0
                        }
                })
            })
            .unwrap_or(0);
        self.publish_load_summary(RunQueueLoadPublication {
            queued_count: queued,
            nr_running,
            fair_demand,
            workload_demand,
            current_workload_demand: current_placement_demand,
            fair_pushable: run_queue.has_pushable_fair(),
            rt_wake_donor,
        })
    }

    /// Reports whether Linux `select_task_rq_rt()` would enter cpupri.
    ///
    /// The rq owner publishes the effective RT donor and `curr` migration
    /// capability in one scalar. Like Linux's unlocked `rq->curr`/`rq->donor`
    /// reads, a racing observation is an optimistic placement hint; the
    /// target rq transaction remains authoritative.
    pub(crate) fn rt_wake_requires_cpupri(&self, wakee_priority: RtPriority) -> bool {
        let publication = self.load.rt_wake_donor.load(Ordering::Acquire);
        let donor_priority = (publication & RT_WAKE_DONOR_PRIORITY_MASK) as u8;
        donor_priority != 0
            && (publication & RT_WAKE_CURRENT_MIGRATION_CAPABLE == 0
                || donor_priority >= wakee_priority.get())
    }

    /// Returns one coherent remotely observable scheduling snapshot.
    ///
    /// The writer owns the target rq and cannot sleep or leave the sequence
    /// odd. Readers therefore wait for the matching even publication just as
    /// Linux load balancing waits for an authoritative rq state. Silently
    /// omitting a CPU after a fixed retry count would turn writer timing into
    /// a second placement policy and could indefinitely suppress balancing.
    pub(crate) fn load_summary(&self) -> CpuLoadSummary {
        loop {
            let epoch = self.load.sequence.load(Ordering::Acquire);
            if epoch & 1 != 0 {
                core::hint::spin_loop();
                continue;
            }
            let queued_count = self.load.queued.load(Ordering::Relaxed);
            let nr_running = self.load.nr_running.load(Ordering::Relaxed);
            let fair_demand = self.load.fair_demand.load(Ordering::Relaxed);
            let workload_demand = self.load.workload_demand.load(Ordering::Relaxed);
            let current_workload_demand = self.load.current_workload_demand.load(Ordering::Relaxed);
            let flags = self.load.flags.load(Ordering::Relaxed);
            if self.load.sequence.load(Ordering::Acquire) != epoch {
                continue;
            }
            return CpuLoadSummary {
                epoch,
                queued_count,
                nr_running,
                fair_demand,
                workload_demand,
                current_workload_demand,
                fair_pushable: flags & SUMMARY_FAIR_PUSHABLE != 0,
            };
        }
    }

    /// Returns the remotely observable pickable candidate count.
    pub(crate) fn queued_summary(&self) -> usize {
        self.load_summary().queued_count()
    }

    pub(crate) fn placement_demand(&self) -> u64 {
        self.load_summary()
            .workload_demand()
            .saturating_add(self.load.incoming_migration_demand.load(Ordering::Acquire))
    }

    /// Returns wake-affine demand after discounting the running waker.
    ///
    /// Linux's synchronous wake-affine path removes `current` from the
    /// waker CPU's effective load because the caller promises to sleep soon.
    /// Incoming migration reservations remain real placement demand.
    pub(crate) fn sync_wake_affine_demand(&self) -> u64 {
        let summary = self.load_summary();
        summary
            .workload_demand()
            .saturating_sub(summary.current_workload_demand())
            .saturating_add(self.load.incoming_migration_demand.load(Ordering::Acquire))
    }

    pub(super) fn reserve_incoming_migration(&self, demand: u64) {
        if self
            .load
            .incoming_migration_demand
            .try_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                current.checked_add(demand)
            })
            .is_err()
        {
            task_runtime::fatal_invariant(
                INCOMING_MIGRATION_OVERFLOW_INVARIANT,
                self.owner.as_u32() as usize,
            );
        }
    }

    pub(crate) fn release_incoming_migration_demand(&self, demand: u64) {
        if demand == 0 {
            return;
        }
        let previous_demand = self
            .load
            .incoming_migration_demand
            .fetch_sub(demand, Ordering::AcqRel);
        if previous_demand < demand {
            task_runtime::fatal_invariant(
                INCOMING_MIGRATION_RELEASE_INVARIANT,
                self.owner.as_u32() as usize,
            );
        }
    }

    #[cfg(any(test, all(axtest, feature = "axtest")))]
    pub(super) fn set_load_summary_sequence_for_test(&self, sequence: u64) {
        self.load.sequence.store(sequence, Ordering::Release);
    }
}
