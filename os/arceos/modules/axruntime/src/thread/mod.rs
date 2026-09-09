//! ArceOS ownership and trait-FFI glue for the OS-independent task system.

use alloc::{boxed::Box, string::String};
use core::{
    pin::Pin,
    ptr,
    sync::atomic::{AtomicBool, AtomicI32, Ordering},
};

use ax_hal::percpu::CpuPin;
use ax_lazyinit::LazyInit;
use ax_task::{
    runtime::{
        RuntimeHandleResult, RuntimeStatus, TaskSystem, TaskSystemHandle,
        config::TaskSystemConfig,
        cpu::{
            CpuLocal, CpuRemote, CpuRemoteHandle, CurrentCpuLocalHandle, CurrentCpuOwnerHandles,
            IrqGuardToken, RuntimeCpuId,
        },
        resource::{
            AddressSpaceDestroyOutcome, AddressSpaceHandle, AddressSpaceMembarrierState,
            AddressSpaceReclaimArmOutcome, ExecutionContextHandle, KernelContextRequest,
            MembarrierRegistration, MembarrierRegistrationPhase, RuntimeMembarrierAction,
            StackHandle, StackRequest, ThreadResources, TlsHandle, UserContextRequest,
        },
        service::SchedulerTickWorkDisposition,
        switch::{
            ContextThreadBinding, CurrentThreadPublication, RuntimeSchedulerFrameEnterResult,
            RuntimeSwitchPlan, SchedSwitchRecord, ThreadIdentityV1,
        },
        task_runtime::impl_trait as impl_task_runtime,
    },
    sched::{CpuId, CpuSet, FairMode, Nice, SchedulePolicy},
    sync::WaitQueue,
    thread::{
        SwitchReason, TaskError, ThreadExtension, ThreadExtensionOps, ThreadHandle, ThreadId,
        ThreadSpec,
        current::{current_thread_extension, current_thread_handle, current_thread_id},
    },
};

mod address_space;
mod mm_activation;
pub use mm_activation::{
    AddressSpaceSwitchProof, SchedulerAddressSpaceActivation, SchedulerAddressSpaceOwner,
    UserAddressSpaceOwner,
};
mod bootstrap;
pub(crate) mod context;

mod lifecycle;
mod resources;
pub(crate) mod runtime_impl;
pub(crate) mod scheduler_events;
mod spawn;
mod thread_resources;
#[cfg(feature = "uspace")]
mod user_entry;

pub use address_space::{
    AddressSpaceCpuState, TaskAddressSpace, detach_current_address_space,
    switch_current_address_space,
};
use address_space::{
    arm_runtime_address_space_reclaim, destroy_runtime_address_space,
    release_current_active_address_space, runtime_address_space_membarrier_state,
    update_runtime_address_space_membarrier_state,
};
#[cfg(feature = "uspace")]
use bootstrap::current_cpu_remote;
#[cfg(kernel_tls)]
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
use context::{
    bind_bootstrap_runtime_context, bind_runtime_context_thread, create_bootstrap_context,
    create_runtime_context, create_user_runtime_context, destroy_runtime_context,
    finish_runtime_context_switch_tail, scheduler_current_thread_identity,
    scheduler_current_thread_publication, switch_runtime_context,
};

pub(crate) fn runtime_task_system_handle() -> TaskSystemHandle {
    task_system().map_or(TaskSystemHandle::NONE, |system| {
        // SAFETY: TASK_SYSTEM owns this pinned allocation through shutdown and
        // exposes it only through shared scheduler APIs.
        unsafe { TaskSystemHandle::from_raw((system as *const TaskSystem).expose_provenance()) }
    })
}

pub(crate) fn scheduler_frame_capabilities(cpu_pin: &CpuPin) -> RuntimeSchedulerFrameEnterResult {
    // SAFETY: the caller claimed the scheduler baton under this same CPU pin;
    // TASK_SYSTEM is initialized before any task may enter the scheduler, and
    // every returned capability is immutable or shutdown-lifetime state tied
    // to that owner CPU and architecture-selected context.
    unsafe {
        RuntimeSchedulerFrameEnterResult::success(
            runtime_task_system_handle(),
            current_cpu_owner_handles(cpu_pin),
        )
    }
}

#[cfg(feature = "uspace")]
pub(crate) fn current_cpu_needs_reschedule_pinned(cpu_pin: &CpuPin) -> Result<bool, TaskError> {
    Ok(current_cpu_remote(cpu_pin)
        .ok_or(TaskError::NotInitialized)?
        .needs_reschedule())
}

#[cfg(kernel_tls)]
use resources::runtime_tls_pointer;
use resources::{
    allocate_runtime_stack, allocate_runtime_tls, deallocate_runtime_stack, deallocate_runtime_tls,
};
pub(crate) use scheduler_events::{on_clock_event, publish_scheduler_tick};
#[cfg(feature = "qperf-metrics")]
pub(crate) use scheduler_events::{
    record_irq_return_scheduler_continuation, record_irq_return_scheduler_window,
};

/// Checks the kernel-thread active-mm membarrier transition in real runtime builds.
#[cfg(axtest)]
pub fn kernel_thread_retains_active_mm_membarrier_state_for_test() -> bool {
    static IDENTITY_ANCHOR: u8 = 0;

    let identity_raw = (&IDENTITY_ANCHOR as *const u8).expose_provenance();
    // SAFETY: the static address is non-zero, unique, and remains live for the
    // complete duration in which this test state can be observed.
    let identity =
        unsafe { ax_task::runtime::resource::AddressSpaceMembarrierId::from_raw(identity_raw) };
    // SAFETY: the identity satisfies the contract above and zero contains no
    // undeclared registration bits.
    let active_mm_state =
        unsafe { ax_task::runtime::resource::AddressSpaceMembarrierState::new(identity, 0) };

    ax_task::runtime::resource::scheduled_membarrier_state_for_test(
        active_mm_state,
        ax_task::runtime::resource::AddressSpaceMembarrierState::NONE,
    ) == active_mm_state
}
/// Resets the current task's user FPU image during a successful executable replacement.
pub fn reset_current_user_fp_state() -> Result<(), TaskError> {
    context::reset_current_user_fp_state()
}

/// Captures the current x86 task's complete standard user xstate image.
#[cfg(all(target_arch = "x86_64", feature = "fp-simd", feature = "uspace"))]
pub fn capture_current_user_fp_state() -> Result<ax_hal::cpu::UserXstate, TaskError> {
    context::capture_current_user_fp_state()
}

/// Replaces the current x86 task's user xstate and physical FPU owner image.
#[cfg(all(target_arch = "x86_64", feature = "fp-simd", feature = "uspace"))]
pub fn replace_current_user_fp_state(state: ax_hal::cpu::UserXstate) -> Result<(), TaskError> {
    context::replace_current_user_fp_state(state)
}
pub use lifecycle::{
    PreparedThread, StagedThread, ThreadOsExtensionBorrow, ThreadOsExtensionLease,
    current_os_extension, exit_current, join_thread, thread_os_extension, wait_thread,
};
#[cfg(test)]
use lifecycle::{
    RUNTIME_THREAD_EXTENSION_OPS, RuntimeExtensionKind, classify_runtime_extension,
    extension_data_after_releasing_lease,
};
use lifecycle::{
    RuntimeThreadData, RuntimeThreadStart, finish_initial_scheduler_switch,
    release_transferred_extension, runtime_thread_entry, runtime_thread_extension,
};
#[cfg(all(feature = "qperf-metrics", any(feature = "ipi", feature = "wake-ipi")))]
pub(crate) use scheduler_events::{record_scheduler_ipi_consume, record_scheduler_ipi_send};
#[cfg(all(target_arch = "x86_64", feature = "fp-simd", feature = "uspace"))]
pub use spawn::prepare_raw_with_extension_in_address_space_and_inherited_fp_scheduler_state;
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
#[cfg(all(test, kernel_tls))]
use thread_resources::assemble_bootstrap_resources;
use thread_resources::{
    InitialContextState, create_bootstrap_resources, create_idle_resources, create_thread_resources,
};
#[cfg(test)]
use thread_resources::{
    ThreadResourceBackend, UnreleasedThreadResources, create_thread_resources_with,
};
#[cfg(feature = "uspace")]
pub use user_entry::UserExecutionContext;

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
