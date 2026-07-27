//! Scheduler configuration and topology identifiers.

/// Default fair scheduling request in nanoseconds.
pub const DEFAULT_FAIR_SLICE_NS: u64 = 1_000_000;
/// Default fair wake-up preemption granularity in nanoseconds.
pub const DEFAULT_WAKEUP_GRANULARITY_NS: u64 = 500_000;
/// Default periodic fair balancing interval in nanoseconds.
pub const DEFAULT_BALANCE_INTERVAL_NS: u64 = 10_000_000;
/// Default round-robin quantum in nanoseconds.
pub const DEFAULT_RR_QUANTUM_NS: u64 = 5_000_000;
/// Default RT bandwidth period in nanoseconds.
pub const DEFAULT_RT_PERIOD_NS: u64 = 1_000_000_000;
/// Default RT runtime budget in nanoseconds.
pub const DEFAULT_RT_RUNTIME_NS: u64 = 950_000_000;
/// Default Deadline admission percentage.
pub const DEFAULT_DEADLINE_CAP_PERCENT: u8 = 95;
/// Default maximum active timers owned by one CPU.
pub const DEFAULT_TIMER_CAPACITY: usize = 4096;
/// Default bounded work budget for scheduler inboxes and timers.
pub const DEFAULT_BATCH_LIMIT: usize = 64;

/// A logical processor identifier in the configured topology.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct CpuId(u32);

impl CpuId {
    /// Creates a logical processor identifier.
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    /// Returns the numeric identifier.
    pub const fn as_u32(self) -> u32 {
        self.0
    }

    /// Returns the identifier as an array index.
    pub const fn as_usize(self) -> usize {
        self.0 as usize
    }
}

/// Immutable sizing and bandwidth policy for one task system.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TaskSystemConfig {
    cpu_count: usize,
    fair_slice_ns: u64,
    wakeup_granularity_ns: u64,
    balance_interval_ns: u64,
    rr_quantum_ns: u64,
    rt_period_ns: u64,
    rt_runtime_ns: u64,
    deadline_cap_percent: u8,
    timer_capacity: usize,
    batch_limit: usize,
}

impl TaskSystemConfig {
    /// Creates a configuration with the project defaults.
    pub const fn new(cpu_count: usize) -> Self {
        Self {
            cpu_count,
            fair_slice_ns: DEFAULT_FAIR_SLICE_NS,
            wakeup_granularity_ns: DEFAULT_WAKEUP_GRANULARITY_NS,
            balance_interval_ns: DEFAULT_BALANCE_INTERVAL_NS,
            rr_quantum_ns: DEFAULT_RR_QUANTUM_NS,
            rt_period_ns: DEFAULT_RT_PERIOD_NS,
            rt_runtime_ns: DEFAULT_RT_RUNTIME_NS,
            deadline_cap_percent: DEFAULT_DEADLINE_CAP_PERCENT,
            timer_capacity: DEFAULT_TIMER_CAPACITY,
            batch_limit: DEFAULT_BATCH_LIMIT,
        }
    }

    /// Returns the topology size.
    pub const fn cpu_count(self) -> usize {
        self.cpu_count
    }

    /// Returns the fair service request.
    pub const fn fair_slice_ns(self) -> u64 {
        self.fair_slice_ns
    }

    /// Returns the fair wake-up granularity.
    pub const fn wakeup_granularity_ns(self) -> u64 {
        self.wakeup_granularity_ns
    }

    /// Returns the balancing interval.
    pub const fn balance_interval_ns(self) -> u64 {
        self.balance_interval_ns
    }

    /// Returns the default round-robin quantum.
    pub const fn rr_quantum_ns(self) -> u64 {
        self.rr_quantum_ns
    }

    /// Returns the RT bandwidth period.
    pub const fn rt_period_ns(self) -> u64 {
        self.rt_period_ns
    }

    /// Returns the RT runtime budget.
    pub const fn rt_runtime_ns(self) -> u64 {
        self.rt_runtime_ns
    }

    /// Returns the Deadline admission cap in percent.
    pub const fn deadline_cap_percent(self) -> u8 {
        self.deadline_cap_percent
    }

    /// Returns the per-CPU active timer capacity.
    pub const fn timer_capacity(self) -> usize {
        self.timer_capacity
    }

    /// Returns the maximum work items processed at one safe point.
    pub const fn batch_limit(self) -> usize {
        self.batch_limit
    }

    /// Overrides the Deadline admission cap.
    pub const fn with_deadline_cap_percent(mut self, percent: u8) -> Self {
        self.deadline_cap_percent = percent;
        self
    }

    /// Overrides the minimum interval between owner-CPU fair migrations.
    pub const fn with_balance_interval_ns(mut self, interval_ns: u64) -> Self {
        self.balance_interval_ns = interval_ns;
        self
    }

    /// Overrides the per-CPU timer capacity.
    pub const fn with_timer_capacity(mut self, capacity: usize) -> Self {
        self.timer_capacity = capacity;
        self
    }

    /// Overrides the bounded scheduler work batch.
    pub const fn with_batch_limit(mut self, limit: usize) -> Self {
        self.batch_limit = limit;
        self
    }
}
