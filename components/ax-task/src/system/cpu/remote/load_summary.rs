use super::*;

const INCOMING_MIGRATION_OVERFLOW_INVARIANT: u32 = 0x4d49_474f;
const INCOMING_MIGRATION_RELEASE_INVARIANT: u32 = 0x4d49_4752;
const RT_WAKE_DONOR_PRIORITY_MASK: u16 = 0x7f;
const RT_WAKE_CURRENT_MIGRATION_CAPABLE: u16 = 1 << 7;

const fn is_sched_idle_rq(nr_running: usize, idle_fair_running: usize) -> bool {
    nr_running != 0 && nr_running == idle_fair_running
}

const fn choose_fair_idle_cpu(
    summary: CpuLoadSummary,
    incoming_migration_demand: u64,
    wakee_is_idle: bool,
) -> bool {
    incoming_migration_demand == 0
        && (summary.nr_running() == 0 || (!wakee_is_idle && summary.fair_idle_only()))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RunQueueLoadPublication {
    queued_count: usize,
    nr_running: usize,
    fair_demand: u64,
    workload_demand: u64,
    current_workload_demand: u64,
    fair_pushable: bool,
    fair_idle_only: bool,
    fair_delayed_count: usize,
    rt_wake_donor: u16,
}

#[derive(Debug)]
pub(super) struct RemoteLoadState {
    queued: AtomicUsize,
    nr_running: AtomicUsize,
    fair_demand: AtomicU64,
    workload_demand: AtomicU64,
    current_workload_demand: AtomicU64,
    incoming_migration_demand: AtomicU64,
    flags: AtomicU16,
    fair_delayed_count: AtomicUsize,
    rt_wake_donor: AtomicU16,
}

impl RemoteLoadState {
    pub(super) const fn new() -> Self {
        Self {
            queued: AtomicUsize::new(0),
            nr_running: AtomicUsize::new(0),
            fair_demand: AtomicU64::new(0),
            workload_demand: AtomicU64::new(0),
            current_workload_demand: AtomicU64::new(0),
            incoming_migration_demand: AtomicU64::new(0),
            flags: AtomicU16::new(0),
            fair_delayed_count: AtomicUsize::new(0),
            rt_wake_donor: AtomicU16::new(0),
        }
    }
}

impl CpuRemote {
    fn publish_load_summary(
        &self,
        previous: Option<RunQueueLoadPublication>,
        publication: RunQueueLoadPublication,
    ) {
        let RunQueueLoadPublication {
            queued_count,
            nr_running,
            fair_demand,
            workload_demand,
            current_workload_demand,
            fair_pushable,
            fair_idle_only,
            fair_delayed_count,
            rt_wake_donor,
        } = publication;
        if previous.is_none_or(|previous| previous.queued_count != queued_count) {
            self.load.queued.store(queued_count, Ordering::Relaxed);
        }
        if previous.is_none_or(|previous| previous.nr_running != nr_running) {
            self.load.nr_running.store(nr_running, Ordering::Relaxed);
        }
        if previous.is_none_or(|previous| previous.fair_demand != fair_demand) {
            self.load.fair_demand.store(fair_demand, Ordering::Relaxed);
        }
        if previous.is_none_or(|previous| previous.workload_demand != workload_demand) {
            self.load
                .workload_demand
                .store(workload_demand, Ordering::Relaxed);
        }
        if previous
            .is_none_or(|previous| previous.current_workload_demand != current_workload_demand)
        {
            self.load
                .current_workload_demand
                .store(current_workload_demand, Ordering::Relaxed);
        }
        let flags = (u16::from(fair_pushable) * SUMMARY_FAIR_PUSHABLE)
            | (u16::from(fair_idle_only) * SUMMARY_FAIR_IDLE_ONLY);
        if previous.is_none_or(|previous| {
            previous.fair_pushable != fair_pushable || previous.fair_idle_only != fair_idle_only
        }) {
            self.load.flags.store(flags, Ordering::Relaxed);
        }
        if previous.is_none_or(|previous| previous.fair_delayed_count != fair_delayed_count) {
            self.load
                .fair_delayed_count
                .store(fair_delayed_count, Ordering::Relaxed);
        }
        if previous.is_none_or(|previous| previous.rt_wake_donor != rt_wake_donor) {
            self.load
                .rt_wake_donor
                .store(rt_wake_donor, Ordering::Release);
        }
    }

    /// Publishes the remotely observable load state while the caller owns this
    /// CPU's runqueue lock.
    ///
    /// Taking the runqueue state by reference keeps queue membership, current
    /// priority, and load publication in one transaction for both owner and
    /// direct remote wake paths. Returns whether the committed load state
    /// changed and therefore advanced the publication sequence.
    pub(crate) fn publish_run_queue_load_summary(&self, run_queue: &mut CpuRunQueueState) -> bool {
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
        // The placement demand is the same queued Fair weight plus the
        // fixed-class contribution.  Keep one Fair-tree read per publication
        // instead of traversing the same aggregate twice on every rq commit.
        let queued_fair_demand = run_queue.fair_demand();
        let fair_demand = queued_fair_demand.saturating_add(current_fair_demand);
        let workload_demand = queued_fair_demand
            .saturating_add(run_queue.fixed_placement_demand())
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
        // Linux `sched_idle_rq()` is exactly
        // `rq->nr_running == rq->cfs.h_nr_idle && rq->nr_running`. Count the
        // unlinked Fair current beside the queue's incremental h_nr_idle so a
        // queued Normal/Batch, RT, Deadline, or stopper task breaks equality.
        let current_is_idle_fair = current.filter(|_| current_non_idle).is_some_and(|current| {
            matches!(
                current.schedule_policy(),
                SchedulePolicy::Fair {
                    mode: FairMode::Idle,
                    ..
                }
            )
        });
        let idle_fair_running = run_queue
            .queued_idle_fair_count()
            .saturating_add(usize::from(current_is_idle_fair));
        let fair_idle_only = is_sched_idle_rq(nr_running, idle_fair_running);
        let fair_delayed_count = run_queue.queued_delayed_fair_count();
        let publication = RunQueueLoadPublication {
            queued_count: queued,
            nr_running,
            fair_demand,
            workload_demand,
            current_workload_demand: current_placement_demand,
            fair_pushable: run_queue.has_pushable_fair(),
            fair_idle_only,
            fair_delayed_count,
            rt_wake_donor,
        };
        let Some((previous, publication)) = run_queue.take_load_publication(publication) else {
            return false;
        };
        self.publish_load_summary(previous, publication);
        true
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

    /// Returns Linux-style lockless placement hints.
    ///
    /// Fields may span adjacent rq commits, just like unlocked `rq` reads in
    /// Linux wake placement. They are hints only; the selected target rq lock
    /// remains the authoritative validation and mutation boundary.
    pub(crate) fn load_summary(&self) -> CpuLoadSummary {
        let flags = self.load.flags.load(Ordering::Relaxed);
        CpuLoadSummary {
            queued_count: self.load.queued.load(Ordering::Relaxed),
            nr_running: self.load.nr_running.load(Ordering::Relaxed),
            fair_demand: self.load.fair_demand.load(Ordering::Relaxed),
            workload_demand: self.load.workload_demand.load(Ordering::Relaxed),
            current_workload_demand: self.load.current_workload_demand.load(Ordering::Relaxed),
            fair_pushable: flags & SUMMARY_FAIR_PUSHABLE != 0,
            fair_idle_only: flags & SUMMARY_FAIR_IDLE_ONLY != 0,
            fair_delayed_count: self.load.fair_delayed_count.load(Ordering::Relaxed),
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

    /// Reports whether Linux `select_idle_sibling()` may use this CPU.
    ///
    /// An incoming migration reservation keeps the CPU busy even when the
    /// published rq is empty or SCHED_IDLE-only. Only a non-idle Fair wakee may
    /// treat the latter as an idle placement target.
    pub(crate) fn is_fair_idle_placement_target(&self, wakee_is_idle: bool) -> bool {
        let summary = self.load_summary();
        choose_fair_idle_cpu(
            summary,
            self.load.incoming_migration_demand.load(Ordering::Acquire),
            wakee_is_idle,
        )
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

    /// Reports Linux's synchronous wake-affine singleton-rq condition.
    pub(crate) fn sync_wake_affine_is_singleton(&self) -> bool {
        let summary = self.load_summary();
        summary
            .nr_running()
            .saturating_sub(summary.fair_delayed_count())
            == 1
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sched_idle_rq_requires_every_runnable_task_to_use_idle_policy() {
        assert!(!is_sched_idle_rq(0, 0));
        assert!(is_sched_idle_rq(1, 1));
        assert!(is_sched_idle_rq(2, 2));
        assert!(!is_sched_idle_rq(2, 1));
        assert!(!is_sched_idle_rq(1, 0));
    }

    #[test]
    fn sched_idle_rq_is_only_an_idle_target_for_non_idle_wakees() {
        let idle_only = load_summary(3, true);
        assert!(choose_fair_idle_cpu(idle_only, 0, false));
        assert!(!choose_fair_idle_cpu(idle_only, 0, true));
        assert!(!choose_fair_idle_cpu(idle_only, 1, false));

        let empty = load_summary(0, false);
        assert!(choose_fair_idle_cpu(empty, 0, false));
        assert!(choose_fair_idle_cpu(empty, 0, true));
    }

    #[test]
    fn running_stopper_is_not_an_idle_placement_target() {
        let stopper = CpuLoadSummary {
            epoch: 0,
            queued_count: 0,
            nr_running: 1,
            fair_demand: 0,
            workload_demand: 0,
            current_workload_demand: 0,
            fair_pushable: false,
            fair_idle_only: false,
            fair_delayed_count: 0,
        };

        assert!(!choose_fair_idle_cpu(stopper, 0, false));
    }

    fn load_summary(workload_demand: u64, fair_idle_only: bool) -> CpuLoadSummary {
        CpuLoadSummary {
            epoch: 0,
            queued_count: 0,
            nr_running: usize::from(fair_idle_only),
            fair_demand: workload_demand,
            workload_demand,
            current_workload_demand: workload_demand,
            fair_pushable: false,
            fair_idle_only,
            fair_delayed_count: 0,
        }
    }
}
