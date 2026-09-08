//! Operating-system capability boundary owned by the scheduler runtime.
//!
//! Runtime resources, clock-domain values, and provider operations are split
//! by owned invariant while retaining one trait-FFI table at the OS boundary.

use crate::runtime::cpu::{IrqGuardToken, PreemptGuardToken};

pub(crate) mod clock;
mod handle;
mod interface;

pub use interface::*;

#[derive(Clone, Copy)]
#[repr(usize)]
pub(crate) enum PreemptGuardSource {
    TicketLock,
    ExplicitScope,
    SyncContext,
    SchedulerActivity,
    IrqReturn,
}

#[derive(Clone, Copy)]
#[repr(usize)]
pub(crate) enum IrqGuardSource {
    ThreadSchedTicket,
    DeadlineServerTicket,
    CpuRunQueueTransactionTicket,
    CpuRunQueueOwnerCurrentThreadObservationTicket,
    CpuRunQueueOwnerCurrentCoreObservationTicket,
    CpuRunQueueOwnerRunnableObservationTicket,
    CpuRunQueueTimerDeadlineDerivationObservationTicket,
    CpuRunQueueRtAccountingTicket,
    CpuRunQueueDeadlineAccountingTicket,
    CpuRunQueueMembarrierTicket,
    CpuRunQueueLifecycleTicket,
    CpuRtBandwidthTicket,
    #[cfg(feature = "qperf-metrics")]
    CpuDeadlineObservationTicket,
    CpuDeadlinePublicationTicket,
    CpuDeadlineRegistrationTicket,
    CpuDeadlineHardExpiryTicket,
    CpuDeadlineSoftExpiryTicket,
    CpuDeadlineLifecycleTicket,
    RootRtRuntimeTicket,
    RootRtPeriodTicket,
    RootDeadlineIndexTicket,
    ExplicitScope,
    RuntimeCpu,
    Executor,
}

pub(crate) fn enter_preempt_guard(source: PreemptGuardSource) -> PreemptGuardToken {
    let token = task_runtime::preempt_guard_enter();
    #[cfg(feature = "qperf-metrics")]
    crate::diagnostics::counters::record_runtime_preempt_guard_entry(source, token.is_none());
    #[cfg(not(feature = "qperf-metrics"))]
    let _ = source;
    token
}

pub(crate) fn enter_irq_guard(source: IrqGuardSource) -> IrqGuardToken {
    let token = task_runtime::irq_guard_enter();
    #[cfg(feature = "qperf-metrics")]
    crate::diagnostics::counters::record_runtime_irq_guard_entry(source, token.is_none());
    #[cfg(not(feature = "qperf-metrics"))]
    let _ = source;
    token
}

pub use crate::sched::system::TaskSystem;

pub mod cpu;

pub mod resource;

pub mod switch;

pub mod service;

pub mod sync;

pub(crate) mod context;

use crate::runtime::handle::opaque_handle;

opaque_handle!(
    /// Opaque pointer-sized handle to the runtime-owned task system.
    TaskSystemHandle,
    "runtime"
);

/// Stable runtime operation status used across the trait-ffi boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum RuntimeStatus {
    /// The operation completed successfully.
    Success         = 0,
    /// The runtime capability has not been initialized.
    NotInitialized  = 1,
    /// A supplied handle is stale or unknown to the runtime.
    InvalidHandle   = 2,
    /// A supplied value violates the runtime contract.
    InvalidArgument = 3,
    /// The runtime cannot allocate the requested resource.
    NoMemory        = 4,
    /// The runtime does not implement this optional capability.
    Unsupported     = 5,
    /// The requested resource is temporarily busy.
    Busy            = 6,
    /// A platform operation failed.
    Platform        = 7,
    /// The caller holds an IRQ/preemption guard or is otherwise non-sleepable.
    UnsafeContext   = 8,
}

/// Result of an operation that creates one opaque runtime resource.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct RuntimeHandleResult {
    /// Completion status.
    pub status: RuntimeStatus,
    /// New resource handle when `status` is [`RuntimeStatus::Success`].
    pub handle: usize,
}

impl RuntimeHandleResult {
    /// Creates a successful handle result.
    pub const fn success(handle: usize) -> Self {
        Self {
            status: RuntimeStatus::Success,
            handle,
        }
    }

    /// Creates a failed handle result.
    pub const fn failure(status: RuntimeStatus) -> Self {
        Self { status, handle: 0 }
    }
}

pub mod config;

pub(crate) mod lock;

pub(crate) mod delivery;
