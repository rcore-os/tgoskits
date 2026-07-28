//! ArceOS ownership and trait-FFI glue for the OS-independent task system.

use alloc::{boxed::Box, string::String};
use core::{
    pin::Pin,
    ptr,
    sync::atomic::{AtomicBool, AtomicI32, AtomicPtr, Ordering},
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

static SCHED_SWITCH_TRACE_HOOK: AtomicPtr<()> = AtomicPtr::new(core::ptr::null_mut());

const PAGE_SIZE: usize = 4096;

/// Allocation-free scheduler-switch diagnostic hook installed by an OS layer.
pub type SchedSwitchTraceHook = fn(SchedSwitchRecord);

/// Installs the process-wide scheduler-switch diagnostic consumer.
///
/// Reinstalling the same function is harmless; replacing a live consumer is an
/// invariant violation because switches may concurrently execute the hook.
pub fn install_sched_switch_trace_hook(hook: SchedSwitchTraceHook) {
    let hook = hook as *mut ();
    match SCHED_SWITCH_TRACE_HOOK.compare_exchange(
        core::ptr::null_mut(),
        hook,
        Ordering::AcqRel,
        Ordering::Acquire,
    ) {
        Ok(_) => {}
        Err(installed) => assert_eq!(installed, hook, "scheduler trace hook already installed"),
    }
}

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

struct ArceOsTaskRuntime;

impl_task_runtime! {
    impl TaskRuntime for ArceOsTaskRuntime {
        unsafe fn task_system_handle() -> TaskSystemHandle {
            task_system().map_or(TaskSystemHandle::NONE, |system| {
                // SAFETY: TASK_SYSTEM owns this pinned allocation through
                // shutdown and exposes it only through shared scheduler APIs.
                unsafe {
                    TaskSystemHandle::from_raw(
                        (system as *const TaskSystem).expose_provenance(),
                    )
                }
            })
        }

        unsafe fn current_cpu_local_handle() -> CurrentCpuLocalHandle {
            // SAFETY: the ax-task caller already owns a CPU pin, and the slot
            // is initialized from the unique pinned CpuLocal allocation before
            // that CPU becomes visible to scheduler entry paths.
            let raw = unsafe { with_current_cpu_pin(current_cpu_local_owner_handle) };
            // SAFETY: zero denotes pre-initialization; every nonzero value is
            // the shutdown-lifetime owner capability installed above.
            unsafe { CurrentCpuLocalHandle::from_raw(raw) }
        }

        unsafe fn cpu_remote_handle(cpu: RuntimeCpuId) -> CpuRemoteHandle {
            cpu_remote(cpu).map_or(CpuRemoteHandle::NONE, |cpu| {
                // SAFETY: TaskSystem owns this Arc-backed CpuRemote endpoint
                // through shutdown and the lookup preserves its CPU identity.
                unsafe {
                    CpuRemoteHandle::from_raw((cpu as *const CpuRemote).expose_provenance())
                }
            })
        }

        fn current_cpu_id() -> RuntimeCpuId {
            let cpu = u32::try_from(ax_hal::percpu::this_cpu_id())
                .expect("logical CPU ID must fit the TaskRuntime ABI");
            RuntimeCpuId::new(cpu)
        }

        fn online_cpu_count() -> u32 {
            task_system()
                .and_then(|system| u32::try_from(system.online_cpu_count()).ok())
                .unwrap_or(0)
        }

        fn irq_guard_enter() -> IrqGuardToken {
            #[cfg(test)]
            {
                // SAFETY: test mode models one balanced runtime IRQ token.
                unsafe { IrqGuardToken::from_raw(1) }
            }
            #[cfg(not(test))]
            {
                crate::guard::enter_irq();
                // SAFETY: enter_irq established the matching live guard state.
                unsafe { IrqGuardToken::from_raw(1) }
            }
        }

        unsafe fn irq_guard_exit(_token: IrqGuardToken) {
            #[cfg(not(test))]
            crate::guard::exit_irq("task runtime");
        }

        fn finish_context_switch_tail() -> RuntimeStatus {
            finish_runtime_context_switch_tail()
        }

        fn finish_initial_context_switch() {
            crate::guard::finish_initial_context_switch();
        }

        fn scheduler_frame_guard_enter(
            origin: ax_task::runtime::RuntimeScheduleOrigin,
            entry: ax_task::runtime::RuntimeSchedulerEntry,
        ) -> RuntimeStatus {
            crate::guard::enter_scheduler_frame_guard(origin, entry)
        }

        fn scheduler_frame_guard_exit(
            return_to: ax_task::runtime::RuntimeSchedulerReturn,
        ) -> bool {
            crate::guard::exit_scheduler_frame_guard(return_to)
        }

        fn in_hard_irq() -> bool {
            #[cfg(test)]
            {
                false
            }
            #[cfg(all(not(test), feature = "irq"))]
            {
                ax_hal::irq::in_irq_context()
            }
            #[cfg(all(not(test), not(feature = "irq")))]
            {
                false
            }
        }

        fn validate_schedule_context(
            origin: ax_task::runtime::RuntimeScheduleOrigin,
        ) -> RuntimeStatus {
            crate::guard::validate_schedule_context(origin)
        }

        fn validate_owner_cpu_context() -> RuntimeStatus {
            crate::guard::validate_owner_cpu_context()
        }

        fn monotonic_ns() -> u64 {
            ax_hal::time::monotonic_time_nanos()
        }

        fn timer_resolution_ns() -> u64 {
            // The four supported architectures expose different counter
            // frequencies. Deriving one representable tick avoids rounding a
            // nanosecond deadline back to the current hardware tick and
            // repeatedly delivering an early interrupt.
            let frequency_hz =
                ax_hal::time::nanos_to_ticks(ax_hal::time::NANOS_PER_SEC);
            crate::timer_resolution_from_frequency(frequency_hz)
        }

        fn publish_task_deadline(
            update: ax_task::runtime::TaskDeadlineUpdate,
        ) -> RuntimeStatus {
            #[cfg(feature = "irq")]
            {
                crate::publish_local_task_deadline(update)
            }
            #[cfg(not(feature = "irq"))]
            {
                let _ = update;
                RuntimeStatus::Unsupported
            }
        }

        fn send_scheduler_ipi(cpu: RuntimeCpuId) -> RuntimeStatus {
            #[cfg(any(feature = "ipi", feature = "wake-ipi"))]
            {
                let cpu_id = cpu.as_u32() as usize;
                if cpu_id >= ax_hal::cpu_num() {
                    return RuntimeStatus::InvalidArgument;
                }
                publish_then_notify_scheduler_ipi(
                    || publish_scheduler_ipi_doorbell(cpu_id),
                    || {
                        ax_hal::irq::send_ipi(
                            ax_hal::irq::ipi_irq(),
                            ax_hal::irq::IpiTarget::Other { cpu_id },
                        );
                    },
                )
            }
            #[cfg(not(any(feature = "ipi", feature = "wake-ipi")))]
            {
                let _ = cpu;
                RuntimeStatus::Unsupported
            }
        }

        fn wait_for_interrupt() {
            ax_hal::asm::disable_irqs();
            let now_ns = ax_hal::time::monotonic_time_nanos();
            let recovered_clockevent = crate::recover_overdue_local_clock_event(now_ns);
            let needs_reschedule = ax_task::current_cpu_needs_resched()
                .expect("idle handoff requires an initialized current CPU");
            if recovered_clockevent
                || needs_reschedule
                || crate::local_clock_event_has_immediate_work(now_ns)
            {
                ax_hal::asm::enable_irqs();
            } else {
                ax_hal::asm::wait_for_irqs_disabled();
            }
        }

        fn allocate_stack(_request: StackRequest) -> RuntimeHandleResult {
            match allocate_runtime_stack(_request) {
                Ok(handle) => RuntimeHandleResult::success(handle.into_raw()),
                Err(status) => RuntimeHandleResult::failure(status),
            }
        }

        fn deallocate_stack(_stack: StackHandle) -> RuntimeStatus {
            deallocate_runtime_stack(_stack)
        }

        fn allocate_tls(_request: TlsRequest) -> RuntimeHandleResult {
            allocate_runtime_tls(_request)
        }

        fn deallocate_tls(_tls: TlsHandle) -> RuntimeStatus {
            deallocate_runtime_tls(_tls)
        }

        fn create_kernel_context(_request: KernelContextRequest) -> RuntimeHandleResult {
            create_runtime_context(_request)
        }

        fn create_user_context(_request: UserContextRequest) -> RuntimeHandleResult {
            create_user_runtime_context(_request)
        }

        fn bind_context_thread(binding: ContextThreadBinding) -> RuntimeStatus {
            bind_runtime_context_thread(binding)
        }

        fn destroy_context(_context: ExecutionContextHandle) -> RuntimeStatus {
            destroy_runtime_context(_context)
        }

        unsafe fn switch_context(
            previous: ExecutionContextHandle,
            next: ExecutionContextHandle,
        ) {
            // SAFETY: the TaskRuntime contract passes the committed previous
            // and next handles under the active scheduler baton.
            unsafe { switch_runtime_context(previous, next) };
        }

        fn install_address_space(address_space: AddressSpaceHandle) -> RuntimeStatus {
            install_runtime_address_space(address_space)
        }

        fn flush_tlb_local(_start: usize, _size: usize) {
            ax_hal::asm::flush_tlb(None);
        }

        fn trace_sched_switch(record: SchedSwitchRecord) {
            let hook = SCHED_SWITCH_TRACE_HOOK.load(Ordering::Acquire);
            if hook.is_null() {
                return;
            }
            // SAFETY: installation accepts exactly this function-pointer type,
            // and the process-wide hook is never replaced or removed.
            let hook = unsafe { core::mem::transmute::<*mut (), SchedSwitchTraceHook>(hook) };
            hook(record);
        }

        fn fatal_invariant(code: u32, argument: usize) -> ! {
            panic!("ax-task invariant {code} failed with argument {argument:#x}")
        }
    }
}

#[cfg(test)]
mod tests;
