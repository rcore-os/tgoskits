/// Scheduler class carried by a remotely observed CPU load summary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum SchedulingClass {
    /// Runtime-owned per-CPU stopper work.
    Stop     = 0,
    /// Absolute-deadline EDF work.
    Deadline = 1,
    /// Fixed-priority FIFO or round-robin work.
    Realtime = 2,
    /// EEVDF work: Normal, Batch, and SCHED_IDLE share this class, matching
    /// Linux's single `cfs_rq`. The per-CPU dedicated idle thread is outside
    /// every summary class because it is never queued or pushed.
    Fair     = 3,
}

/// Allocation-free lockless hints used by remote placement and balancing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CpuLoadSummary {
    pub(super) queued_count: usize,
    pub(super) nr_running: usize,
    pub(super) fair_demand: u64,
    pub(super) workload_demand: u64,
    pub(super) current_workload_demand: u64,
    pub(super) fair_pushable: bool,
    pub(super) fair_idle_only: bool,
    pub(super) fair_delayed_count: usize,
}

/// Per-runqueue GRUB utilization snapshot in billionths of one CPU.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeadlineBandwidthSnapshot {
    pub(super) this_bw_scaled: u64,
    pub(super) running_bw_scaled: u64,
    pub(super) max_bw_scaled: u64,
}

impl DeadlineBandwidthSnapshot {
    pub(crate) const fn new(
        this_bw_scaled: u64,
        running_bw_scaled: u64,
        max_bw_scaled: u64,
    ) -> Self {
        Self {
            this_bw_scaled,
            running_bw_scaled,
            max_bw_scaled,
        }
    }

    /// Returns all Deadline utilization assigned to this runqueue.
    pub const fn this_bw_scaled(self) -> u64 {
        self.this_bw_scaled
    }

    /// Returns ActiveContending plus ActiveNonContending utilization.
    pub const fn running_bw_scaled(self) -> u64 {
        self.running_bw_scaled
    }

    /// Returns utilization currently eligible for GRUB reclaim.
    pub const fn inactive_bw_scaled(self) -> u64 {
        assert!(self.running_bw_scaled <= self.this_bw_scaled);
        self.this_bw_scaled - self.running_bw_scaled
    }

    /// Returns the per-CPU reclaim capacity.
    pub const fn max_bw_scaled(self) -> u64 {
        self.max_bw_scaled
    }
}

impl CpuLoadSummary {
    /// Returns candidates available to `pick_next_task()`, excluding current.
    pub const fn queued_count(self) -> usize {
        self.queued_count
    }

    /// Returns Linux `rq->nr_running`, including a non-idle current task.
    pub const fn nr_running(self) -> usize {
        self.nr_running
    }

    /// Returns the Linux nice-weighted Fair demand owned by this CPU.
    pub const fn fair_demand(self) -> u64 {
        self.fair_demand
    }

    /// Returns instantaneous scheduling demand in normal-nice weight units.
    ///
    /// Fair work contributes its exact nice weight. RT and Deadline work each
    /// contribute one normal-nice capacity unit until class-specific
    /// utilization tracking is available.
    pub const fn workload_demand(self) -> u64 {
        self.workload_demand
    }

    /// Returns the demand contributed by this CPU's non-idle current task.
    pub const fn current_workload_demand(self) -> u64 {
        self.current_workload_demand
    }

    /// Reports whether this CPU has migratable Fair work.
    ///
    /// RT and Deadline overload are deliberately absent from this load
    /// snapshot. Their sole remote authority is the root-domain `rto`/`dlo`
    /// index, matching Linux's separation between sched-domain load and
    /// priority-class overload state.
    pub const fn has_pushable_fair(self) -> bool {
        self.fair_pushable
    }

    /// Reports whether every runnable task on this CPU uses SCHED_IDLE.
    ///
    /// Mirrors Linux `sched_idle_rq()`: a non-idle Fair wakee may treat this
    /// rq as an idle placement target.
    pub const fn fair_idle_only(self) -> bool {
        self.fair_idle_only
    }

    /// Returns Linux `cfs_rq->h_nr_delayed` for this flat runqueue.
    pub const fn fair_delayed_count(self) -> usize {
        self.fair_delayed_count
    }
}

pub(super) const SUMMARY_FAIR_PUSHABLE: u16 = 1 << 0;
pub(super) const SUMMARY_FAIR_IDLE_ONLY: u16 = 1 << 1;
