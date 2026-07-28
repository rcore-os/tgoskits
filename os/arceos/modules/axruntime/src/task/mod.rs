//! ArceOS ownership and trait-FFI glue for the OS-independent task system.

use alloc::{boxed::Box, string::String};
use core::{
    pin::Pin,
    ptr,
    sync::atomic::{AtomicBool, AtomicI32, Ordering},
};

use ax_hal::percpu::CpuPin;
use ax_kernel_guard::IrqSave;
use ax_kspin::SpinNoIrq;
use ax_lazyinit::LazyInit;
pub use ax_task::{
    CpuId, CpuSet, DeadlineFlags, DeadlinePolicy, FairMode, IrqRegisterResult, IrqUnregisterResult,
    IrqWaitCell, IrqWaitRegistration, IrqWaitToken, Nice, RtPriority, SchedulePolicy, SwitchReason,
    TaskError, ThreadExtension, ThreadExtensionOps, ThreadHandle, ThreadId, ThreadState,
    ThreadWakeHandle, WaitQueue, WakeResult, cpu_busy_runtime_ns, current_cpu_needs_resched,
    current_thread_extension, current_thread_handle, current_thread_id, executor::LocalExecutor,
    exit_current_thread, quiesce_irq_wait, runtime::SchedSwitchRecord, schedule_current_cpu,
    set_current_thread_affinity, set_thread_affinity, set_thread_policy, sleep, sleep_until,
    thread_affinity, thread_handle, thread_policy, thread_round_robin_interval_ns, thread_runtime,
    yield_current_cpu,
};
use ax_task::{
    CpuLocal, CpuRemote, TaskSystem, TaskSystemConfig, ThreadResources, ThreadSpec,
    impl_trait as impl_task_runtime,
    runtime::{
        AddressSpaceHandle, ContextThreadBinding, CpuRemoteHandle, CurrentCpuLocalHandle,
        ExecutionContextHandle, IrqGuardToken, KernelContextRequest, RuntimeCpuId,
        RuntimeHandleResult, RuntimeStatus, StackHandle, StackRequest, TaskRuntime,
        TaskSystemHandle, TlsHandle, TlsRequest, UserContextRequest,
    },
};

mod bootstrap;
mod context;
mod executor;
mod resources;
mod runtime_impl;
mod scheduler_events;
mod spawn;
mod thread;
mod thread_resources;

#[cfg(feature = "tls")]
pub(crate) use bootstrap::initialize_early_bootstrap_tls;
#[cfg(test)]
use bootstrap::{IdleEntryAction, idle_entry_action};
pub(crate) use bootstrap::{
    PublishedCpuOnline, current_cpu_remote, initialize_primary, publish_current_cpu_online,
    start_deferred_task_work_service,
};
use bootstrap::{
    cpu_remote, current_cpu_local_owner_handle, idle_context_entry, primary_bootstrap_thread,
    task_system, with_current_cpu_local_mut_owner, with_current_cpu_pin,
};
#[cfg(feature = "smp")]
pub(crate) use bootstrap::{initialize_secondary, run_idle};
pub use context::{
    TaskAddressSpace, diagnose_current_stack_guard_page_fault, switch_current_page_table,
};
use context::{
    bind_bootstrap_runtime_context, bind_runtime_context_thread, create_bootstrap_context,
    create_runtime_context, create_user_runtime_context, destroy_runtime_context,
    finish_runtime_context_switch_tail, install_runtime_address_space, switch_runtime_context,
};
pub use executor::{BlockOnError, block_on, block_on_timeout};
#[cfg(feature = "tls")]
use resources::runtime_tls_pointer;
use resources::{
    allocate_runtime_stack, allocate_runtime_tls, deallocate_runtime_stack, deallocate_runtime_tls,
};
pub use runtime_impl::{SchedSwitchTraceHook, install_sched_switch_trace_hook};
#[cfg(test)]
use scheduler_events::clock_event_requests_reschedule;
#[cfg(all(test, not(any(feature = "ipi", feature = "wake-ipi"))))]
use scheduler_events::publish_then_notify_scheduler_ipi;
pub use scheduler_events::timer_irq_count;
#[cfg(any(feature = "ipi", feature = "wake-ipi"))]
pub(crate) use scheduler_events::{consume_scheduler_ipi_doorbell, on_scheduler_ipi};
#[cfg(feature = "irq")]
pub(crate) use scheduler_events::{on_clock_event, recover_clock_event};
#[cfg(any(feature = "ipi", feature = "wake-ipi"))]
use scheduler_events::{publish_scheduler_ipi_doorbell, publish_then_notify_scheduler_ipi};
#[cfg(all(target_arch = "riscv64", feature = "fp-simd"))]
pub use spawn::{
    prepare_raw_with_extension_in_address_space_and_fp_state_and_policy,
    spawn_raw_with_extension_in_address_space_and_fp_state,
    spawn_raw_with_extension_in_address_space_and_fp_state_and_policy,
};
pub use spawn::{
    prepare_raw_with_extension_in_address_space_and_policy, spawn_raw, spawn_raw_with_affinity,
    spawn_raw_with_extension, spawn_raw_with_extension_and_affinity,
    spawn_raw_with_extension_in_address_space,
    spawn_raw_with_extension_in_address_space_and_policy,
};
pub use thread::{
    PreparedThread, ThreadOsExtensionBorrow, ThreadOsExtensionLease, current_os_extension,
    exit_current, join_thread, thread_os_extension, wait_thread,
};
use thread::{
    RUNTIME_THREAD_EXTENSION_OPS, RuntimeThreadData, finish_initial_scheduler_switch,
    release_transferred_extension, runtime_thread_entry,
};
#[cfg(test)]
use thread::{
    RuntimeExtensionKind, classify_runtime_extension, extension_data_after_releasing_lease,
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
