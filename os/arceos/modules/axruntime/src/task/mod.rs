//! ArceOS ownership and trait-FFI glue for the OS-independent task system.

use alloc::{boxed::Box, string::String};
use core::{
    pin::Pin,
    ptr,
    sync::atomic::{AtomicBool, AtomicI32, Ordering},
};

use ax_hal::percpu::CpuPin;
use ax_lazyinit::LazyInit;
pub use ax_task::{
    CpuId, CpuSet, CurrentParkDisposition, CurrentParkResume, CurrentParkStart, CurrentThreadToken,
    DeadlineFlags, DeadlinePolicy, FairMode, HardKernelTimerAction, HardKernelTimerCallback,
    IrqRegisterResult, IrqWaitCell, IrqWaitRegistration, IrqWaitToken, IrqWorkerWaiter,
    KernelTimerAction, KernelTimerCancelOutcome, KernelTimerHandle, MembarrierCommand,
    MembarrierError, Nice, PreparedCurrentPark, RtPriority, SchedulePolicy, SchedulerTickCpuTime,
    SchedulerTickCpuTimeSnapshot, SchedulerTickGate, SchedulerTickMode, SchedulerTickTaskWork,
    SchedulerTickWorkDisposition, SwitchReason, TaskError, ThreadExtension, ThreadExtensionOps,
    ThreadHandle, ThreadId, ThreadState, ThreadWakeBatch, ThreadWakeHandle, WaitQueue, WakeResult,
    active_cpu_set, arm_hard_kernel_timer, begin_current_park, cancel_kernel_timer,
    cpu_busy_runtime_ns, cpu_topology_len, current_cpu_needs_resched, current_thread_extension,
    current_thread_handle, current_thread_id, current_thread_token, disarm_hard_kernel_timer,
    executor::{LocalExecutor, wake_waker_sync},
    exit_current_thread, membarrier, quiesce_irq_wait, register_current_membarrier,
    register_hard_restartable_kernel_timer, register_kernel_timer,
    register_restartable_kernel_timer,
    runtime::{MembarrierRegistration, MonotonicDeadline, MonotonicInstant, SchedSwitchRecord},
    schedule_current_cpu, set_current_thread_affinity, set_thread_affinity,
    set_thread_affinity_and_wait, set_thread_policy, sleep, sleep_until, thread_affinity,
    thread_handle, thread_policy, thread_runtime, validate_blocking_context, yield_current_cpu,
};

/// Arms the shared physical IPI delivery edge for one CPU.
///
/// Callers must publish their logical pending state before this doorbell. The
/// shared edge coalesces repeated notifications until the target CPU claims
/// the physical interrupt; logical owners remain responsible for draining
/// their own state.
pub fn notify_cpu(cpu_id: usize) -> Result<(), ax_hal::irq::IrqError> {
    #[cfg(any(feature = "ipi", feature = "wake-ipi"))]
    {
        if cpu_id >= ax_hal::cpu_num() {
            return Err(ax_hal::irq::IrqError::InvalidCpu);
        }
        ax_ipi::notify_cpu(ax_hal::irq::CpuId(cpu_id)).map(|_| ())
    }
    #[cfg(not(any(feature = "ipi", feature = "wake-ipi")))]
    {
        let _ = cpu_id;
        Err(ax_hal::irq::IrqError::Unsupported)
    }
}

use ax_task::{
    CpuLocal, CpuRemote, TaskSystem, TaskSystemConfig, ThreadResources, ThreadSpec,
    impl_trait as impl_task_runtime,
    runtime::{
        AddressSpaceActivation, AddressSpaceDestroyOutcome, AddressSpaceHandle,
        AddressSpaceMembarrierState, AddressSpaceReclaimArmOutcome, ContextSwitch,
        ContextThreadBinding, CpuRemoteHandle, CurrentCpuLocalHandle, CurrentCpuOwnerHandles,
        CurrentThreadPublication, ExecutionContextHandle, IrqGuardToken, KernelContextRequest,
        MembarrierRegistrationPhase, RuntimeCpuId, RuntimeHandleResult, RuntimeMembarrierAction,
        RuntimeStatus, StackHandle, StackRequest, TaskRuntime, TaskSystemHandle, TlsHandle,
        TlsRequest, UserContextRequest,
    },
};

mod address_space;
mod bootstrap;
mod context;
mod executor;
mod irq_worker;
mod resources;
mod runtime_impl;
mod scheduler_events;
mod spawn;
mod thread;
mod thread_resources;

pub use address_space::{
    AddressSpaceCpuState, TaskAddressSpace, detach_current_address_space,
    switch_current_address_space,
};
use address_space::{
    activate_runtime_address_space, arm_runtime_address_space_reclaim,
    destroy_runtime_address_space, release_current_active_address_space,
    runtime_address_space_membarrier_state, update_runtime_address_space_membarrier_state,
};
#[cfg(feature = "tls")]
pub(crate) use bootstrap::initialize_early_bootstrap_tls;
#[cfg(test)]
use bootstrap::{IdleEntryAction, idle_entry_action};
pub(crate) use bootstrap::{
    PublishedCpuOnline, initialize_primary, publish_current_cpu_online,
    start_current_ktimer_service, start_deferred_task_work_service,
};
use bootstrap::{
    cpu_remote, current_cpu_owner_handles, idle_context_entry, primary_bootstrap_thread,
    scheduler_current_cpu_remote_handle, task_system, with_current_cpu_local_mut_owner,
    with_current_cpu_pin,
};
#[cfg(feature = "smp")]
pub(crate) use bootstrap::{initialize_secondary, run_idle};
pub use context::diagnose_current_stack_guard_page_fault;
use context::{
    bind_bootstrap_runtime_context, bind_runtime_context_thread, create_bootstrap_context,
    create_runtime_context, create_user_runtime_context, destroy_runtime_context,
    finish_runtime_context_switch_tail, scheduler_current_thread_publication,
    switch_runtime_context,
};
pub use executor::{BlockOnError, block_on, block_on_timeout};
pub use irq_worker::FixedIrqWorkerSignal;
#[cfg(feature = "tls")]
use resources::runtime_tls_pointer;
use resources::{
    allocate_runtime_stack, allocate_runtime_tls, deallocate_runtime_stack, deallocate_runtime_tls,
};
pub use runtime_impl::{SchedSwitchTraceHook, install_sched_switch_trace_hook};
pub use scheduler_events::timer_irq_count;
#[cfg(feature = "qperf-metrics")]
pub use scheduler_events::{
    QperfRuntimeSchedulerMetricsSnapshot, qperf_runtime_scheduler_metrics_snapshot,
};
#[cfg(feature = "irq")]
pub(crate) use scheduler_events::{on_clock_event, publish_scheduler_tick};

/// Drains scheduler work and leaves IRQs disabled for atomic userspace entry.
///
/// The caller must invoke the architecture `UserContext::run()` immediately
/// after this succeeds; that path restores the saved userspace IRQ state.
pub fn prepare_user_return() -> Result<(), TaskError> {
    crate::guard::prepare_user_return()
}
#[cfg(all(feature = "qperf-metrics", any(feature = "ipi", feature = "wake-ipi")))]
pub(crate) use scheduler_events::{record_scheduler_ipi_consume, record_scheduler_ipi_send};
pub use spawn::{
    prepare_raw, prepare_raw_with_extension_in_address_space_and_scheduler_state, spawn_raw,
    spawn_raw_with_affinity, spawn_raw_with_extension, spawn_raw_with_extension_and_affinity,
    spawn_raw_with_extension_in_address_space,
    spawn_raw_with_extension_in_address_space_and_policy, spawn_raw_with_policy_and_affinity,
};
#[cfg(all(target_arch = "riscv64", feature = "fp-simd"))]
pub use spawn::{
    prepare_raw_with_extension_in_address_space_and_fp_scheduler_state,
    spawn_raw_with_extension_in_address_space_and_fp_state,
    spawn_raw_with_extension_in_address_space_and_fp_state_and_policy,
};
pub use thread::{
    PreparedThread, StagedThread, ThreadOsExtensionBorrow, ThreadOsExtensionLease,
    current_os_extension, exit_current, join_thread, thread_os_extension, wait_thread,
};
#[cfg(test)]
use thread::{
    RUNTIME_THREAD_EXTENSION_OPS, RuntimeExtensionKind, classify_runtime_extension,
    extension_data_after_releasing_lease,
};
use thread::{
    RuntimeThreadData, RuntimeThreadStart, finish_initial_scheduler_switch,
    release_transferred_extension, runtime_thread_entry, runtime_thread_extension,
};
#[cfg(all(test, feature = "tls"))]
use thread_resources::assemble_bootstrap_resources;
use thread_resources::{
    InitialContextState, create_bootstrap_resources, create_idle_resources, create_thread_resources,
};
#[cfg(test)]
use thread_resources::{
    ThreadResourceBackend, UnreleasedThreadResources, create_thread_resources_with,
};

const PAGE_SIZE: usize = 4096;

#[cfg(not(feature = "fs"))]
const DEFAULT_TASK_STACK_SIZE: usize = 256 * 1024;

const fn runtime_status_error(status: RuntimeStatus) -> TaskError {
    TaskError::RuntimeFailure(status as u32)
}

const fn runtime_task_stack_size() -> usize {
    #[cfg(feature = "fs")]
    {
        crate::build_info::TASK_STACK_SIZE
    }
    #[cfg(not(feature = "fs"))]
    {
        DEFAULT_TASK_STACK_SIZE
    }
}

/// Returns the kernel stack size used by ordinary runtime threads.
pub const fn default_task_stack_size() -> usize {
    runtime_task_stack_size()
}

#[cfg(test)]
mod tests;
