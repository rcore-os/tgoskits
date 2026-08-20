//! Operating-system capability boundary owned by the scheduler runtime.
//!
//! Runtime resources, clock-domain values, and provider operations are split
//! by owned invariant while retaining one trait-FFI table at the OS boundary.
mod capability;
mod clock;
mod interface;

pub use capability::*;
pub use clock::*;
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
    CpuDeadlineObservationTicket,
    CpuDeadlinePublicationTicket,
    CpuDeadlineRegistrationTicket,
    CpuDeadlineHardExpiryTicket,
    CpuDeadlineSoftExpiryTicket,
    CpuDeadlineLifecycleTicket,
    RootRtRuntimeTicket,
    RootRtPeriodTicket,
    #[cfg(feature = "task-test-hooks")]
    RootRtPeriodDeadlineObservationTicket,
    RootDeadlineIndexTicket,
    ExplicitScope,
    RuntimeCpu,
    Executor,
}

pub(crate) fn enter_preempt_guard(source: PreemptGuardSource) -> PreemptGuardToken {
    let token = task_runtime::preempt_guard_enter();
    #[cfg(feature = "qperf-metrics")]
    crate::metrics::record_runtime_preempt_guard_entry(source, token.is_none());
    #[cfg(not(feature = "qperf-metrics"))]
    let _ = source;
    token
}

pub(crate) fn enter_irq_guard(source: IrqGuardSource) -> IrqGuardToken {
    let token = task_runtime::irq_guard_enter();
    #[cfg(feature = "task-test-hooks")]
    if source as usize == IrqGuardSource::RootRtPeriodDeadlineObservationTicket as usize {
        // SAFETY: the runtime IRQ token acquired above pins this operation to
        // the calling CPU until the matching guard exits.
        let cpu = crate::CpuId::new(unsafe { task_runtime::current_cpu_id() }.as_u32());
        crate::task_test_hooks::record_deadline_rt_period_lock_entry(cpu);
    }
    #[cfg(feature = "qperf-metrics")]
    crate::metrics::record_runtime_irq_guard_entry(source, token.is_none());
    #[cfg(not(feature = "qperf-metrics"))]
    let _ = source;
    token
}
