//! Errors returned by the scheduler model.

/// Errors produced by task-system operations.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum TaskError {
    /// A task-system configuration field is internally inconsistent.
    #[error("invalid task-system configuration")]
    InvalidConfiguration,
    /// The requested CPU count is zero or cannot be represented.
    #[error("invalid CPU count: {0}")]
    InvalidCpuCount(usize),
    /// A CPU identifier is outside the configured topology.
    #[error("CPU {0} is outside the configured topology")]
    InvalidCpu(u32),
    /// A CPU-local object was used by a CPU other than its owner.
    #[error("CPU-local owner mismatch: expected {expected}, got {actual}")]
    CpuOwnerMismatch {
        /// CPU that owns the object.
        expected: u32,
        /// CPU attempting the operation.
        actual: u32,
    },
    /// The calling CPU's scheduler object already has an active owner borrow.
    #[error("CPU-local scheduler owner is already borrowed")]
    CpuOwnerBorrowed,
    /// The CPU is already registered or online.
    #[error("CPU {0} is already online")]
    CpuAlreadyOnline(u32),
    /// The CPU is not online.
    #[error("CPU {0} is not online")]
    CpuOffline(u32),
    /// A nice value is outside `-20..=19`.
    #[error("invalid nice value: {0}")]
    InvalidNice(i8),
    /// An RT priority is outside `1..=99`.
    #[error("invalid real-time priority: {0}")]
    InvalidRtPriority(u8),
    /// A round-robin quantum is zero.
    #[error("round-robin quantum must be non-zero")]
    InvalidRoundRobinQuantum,
    /// Deadline parameters violate `0 < runtime <= deadline <= period`.
    #[error(
        "invalid deadline parameters: runtime={runtime_ns}, deadline={deadline_ns}, \
         period={period_ns}"
    )]
    InvalidDeadline {
        /// Reserved runtime in nanoseconds.
        runtime_ns: u64,
        /// Relative deadline in nanoseconds.
        deadline_ns: u64,
        /// Replenishment period in nanoseconds.
        period_ns: u64,
    },
    /// Deadline flags contain unsupported bits.
    #[error("unsupported deadline flags: {0:#x}")]
    UnsupportedDeadlineFlags(u32),
    /// A deadline reservation exceeds root-domain capacity.
    #[error("deadline admission would exceed the configured root-domain capacity")]
    DeadlineAdmission,
    /// Deadline affinity does not cover every online CPU.
    #[error("deadline affinity must cover the complete online root domain")]
    DeadlineAffinity,
    /// The thread identifier is stale or unknown.
    #[error("unknown or stale thread identifier")]
    StaleThreadId,
    /// A local executor wake header does not belong to the calling thread.
    #[error("executor owner mismatch: expected thread {expected:#x}, got {actual:#x}")]
    ExecutorOwnerMismatch {
        /// Thread encoded in the direct wake header.
        expected: u64,
        /// Calling scheduler thread.
        actual: u64,
    },
    /// The requested lifecycle transition is invalid.
    #[error("invalid thread-state transition from {from:?} to {to:?}")]
    InvalidTransition {
        /// State before the attempted transition.
        from: crate::ThreadState,
        /// Requested state.
        to: crate::ThreadState,
    },
    /// A thread is already present in a run queue.
    #[error("thread is already queued")]
    AlreadyQueued,
    /// A thread is not in a schedulable state.
    #[error("thread is not ready to be queued")]
    NotReady,
    /// A thread cannot be reaped before reaching `Exited`.
    #[error("thread must be exited before it can be reaped")]
    NotExited,
    /// The CPU has no idle thread and no runnable work.
    #[error("CPU has no runnable or idle thread")]
    NoRunnableThread,
    /// A configured fixed-capacity resource is exhausted.
    #[error("timer capacity is exhausted")]
    TimerCapacity,
    /// An affinity update would move a thread away from its owner-CPU sleep timer.
    #[error("thread affinity excludes the CPU owning an active sleep timer")]
    ActiveTimerAffinity,
    /// A live current-CPU lease prevents changing this thread's placement.
    #[error("thread placement is pinned to its current CPU")]
    ThreadPinned,
    /// The runtime-backed facade has not been initialized.
    #[error("task runtime is not initialized")]
    NotInitialized,
    /// A runtime handle is non-null but violates the object handle contract.
    #[error("runtime returned an invalid task object handle")]
    InvalidRuntimeHandle,
    /// Scheduling was requested from hard IRQ or another non-safe-point context.
    #[error("operation requires a scheduler safe point")]
    UnsafeContext,
    /// PI ownership or waiter metadata does not match the requested transition.
    #[error("invalid priority-inheritance state")]
    InvalidPiState,
    /// A transitive priority donation would form a lock dependency cycle.
    #[error("priority-inheritance dependency cycle")]
    PiCycle,
    /// Thread wake/handle references still retain runtime-owned resources.
    #[error("thread resources are still referenced")]
    ThreadBusy,
    /// A runtime resource teardown operation failed.
    #[error("runtime resource operation failed with status {0}")]
    RuntimeFailure(u32),
}
