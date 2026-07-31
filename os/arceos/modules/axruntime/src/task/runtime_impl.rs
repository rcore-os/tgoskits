use core::sync::atomic::{AtomicPtr, Ordering};

use ax_task::runtime::PreemptGuardToken;

use super::*;

static SCHED_SWITCH_TRACE_HOOK: AtomicPtr<()> = AtomicPtr::new(core::ptr::null_mut());

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

        unsafe fn current_cpu_remote_handle() -> CpuRemoteHandle {
            // SAFETY: the ax-task caller keeps the scheduler-owned current
            // thread fixed. Bootstrap cached this CPU's Arc-backed endpoint
            // before online publication and retains its TaskSystem owner.
            let raw = unsafe { scheduler_current_cpu_remote_handle() };
            // SAFETY: zero denotes pre-initialization; every nonzero value is
            // the shutdown-lifetime current-CPU endpoint cached above.
            unsafe { CpuRemoteHandle::from_raw(raw) }
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

        unsafe fn current_cpu_id() -> RuntimeCpuId {
            let cpu = u32::try_from(unsafe { ax_hal::percpu::scheduler_current_cpu_id() })
                .expect("logical CPU ID must fit the TaskRuntime ABI");
            RuntimeCpuId::new(cpu)
        }

        fn online_cpu_count() -> u32 {
            task_system()
                .and_then(|system| u32::try_from(system.online_cpu_count()).ok())
                .unwrap_or(0)
        }

        fn prepare_cpu_online(cpu: RuntimeCpuId) -> RuntimeStatus {
            // SAFETY: this hook runs on the IRQ-excluded owner CPU before
            // scheduler publication.
            if cpu != unsafe { Self::current_cpu_id() } {
                return RuntimeStatus::InvalidArgument;
            }
            #[cfg(feature = "irq")]
            crate::clock_event_runtime::init_timer();
            RuntimeStatus::Success
        }

        fn prepare_cpu_offline(cpu: RuntimeCpuId) -> RuntimeStatus {
            // SAFETY: this hook runs on the IRQ-excluded owner CPU after
            // remote admission has closed.
            if cpu != unsafe { Self::current_cpu_id() } {
                return RuntimeStatus::InvalidArgument;
            }
            #[cfg(feature = "irq")]
            crate::clock_event_runtime::take_current_clock_event_offline();
            RuntimeStatus::Success
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

        fn preempt_guard_enter() -> PreemptGuardToken {
            #[cfg(test)]
            {
                // SAFETY: test mode models one balanced runtime preemption token.
                unsafe { PreemptGuardToken::from_raw(1) }
            }
            #[cfg(not(test))]
            {
                crate::guard::enter_preempt();
                // SAFETY: enter_preempt established the matching live depth.
                unsafe { PreemptGuardToken::from_raw(1) }
            }
        }

        unsafe fn preempt_guard_exit(_token: PreemptGuardToken) {
            #[cfg(not(test))]
            crate::guard::exit_preempt();
        }

        fn local_scheduler_work_is_self_serviced() -> bool {
            #[cfg(test)]
            {
                false
            }
            #[cfg(not(test))]
            {
                crate::guard::local_scheduler_work_is_self_serviced()
            }
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
            crate::clock_event_runtime::timer_resolution_from_frequency(frequency_hz)
        }

        fn publish_task_deadline(
            update: ax_task::runtime::TaskDeadlineUpdate,
        ) -> RuntimeStatus {
            #[cfg(feature = "irq")]
            {
                crate::clock_event_runtime::publish_local_task_deadline(update)
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
            let recovered_clockevent =
                crate::clock_event_runtime::recover_overdue_local_clock_event(now_ns);
            let needs_reschedule = ax_task::current_cpu_needs_resched()
                .expect("idle handoff requires an initialized current CPU");
            if recovered_clockevent
                || needs_reschedule
                || crate::clock_event_runtime::local_clock_event_has_immediate_work(now_ns)
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
