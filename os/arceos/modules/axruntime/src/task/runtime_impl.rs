use core::sync::atomic::{AtomicPtr, Ordering};

use ax_task::runtime::{LocalIrqState, PreemptGuardToken};

use super::*;

static SCHED_SWITCH_TRACE_HOOK: AtomicPtr<()> = AtomicPtr::new(core::ptr::null_mut());

unsafe fn membarrier_ipi_memory_barrier(_arg: *mut ()) {
    core::sync::atomic::fence(Ordering::SeqCst);
}

unsafe fn membarrier_ipi_refresh_run_queue(_arg: *mut ()) {
    ax_task::refresh_current_membarrier_run_queue()
        .unwrap_or_else(|error| panic!("membarrier rq refresh failed in IPI: {error}"));
}

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

        unsafe fn current_cpu_owner_handles() -> CurrentCpuOwnerHandles {
            // SAFETY: the ax-task caller already owns a migration pin. The
            // callback captures the ID and both endpoints from that same CPU
            // area in one transaction.
            unsafe { with_current_cpu_pin(current_cpu_owner_handles) }
        }

        unsafe fn current_cpu_remote_handle() -> CpuRemoteHandle {
            // SAFETY: the ax-task caller keeps the scheduler-owned current
            // thread fixed. Bootstrap cached this CPU's Arc-backed endpoint
            // before online publication and retains its TaskSystem owner.
            unsafe { scheduler_current_cpu_remote_handle() }
        }

        fn current_thread_publication() -> CurrentThreadPublication {
            scheduler_current_thread_publication()
        }

        fn current_preemption_pending() -> bool {
            cpu_local::current_preemption_pending().unwrap_or_else(|error| {
                panic!("current preemption state is unavailable: {error}")
            })
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
            // SAFETY: the TaskRuntime caller retains a migration pin for the
            // complete owner-CPU observation.
            let cpu = unsafe { with_current_cpu_pin(|pin| pin.area().cpu_index().as_u32()) };
            RuntimeCpuId::new(cpu)
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
            release_current_active_address_space();
            RuntimeStatus::Success
        }

        fn local_irq_save_and_disable() -> LocalIrqState {
            let was_enabled = ax_hal::asm::irqs_enabled();
            ax_hal::asm::disable_irqs();
            // SAFETY: the provider restores only the boolean state encoded by
            // this implementation's matching restore operation.
            unsafe { LocalIrqState::from_raw(usize::from(was_enabled)) }
        }

        unsafe fn local_irq_restore(state: LocalIrqState) {
            if state.into_raw() != 0 {
                ax_hal::asm::enable_irqs();
            } else {
                ax_hal::asm::disable_irqs();
            }
        }

        fn irq_guard_enter() -> IrqGuardToken {
            #[cfg(any(test, feature = "host-test"))]
            {
                // SAFETY: host-test mode models one balanced runtime IRQ token.
                unsafe { IrqGuardToken::from_raw(1) }
            }
            #[cfg(not(any(test, feature = "host-test")))]
            {
                crate::guard::enter_irq();
                // SAFETY: enter_irq established the matching live guard state.
                unsafe { IrqGuardToken::from_raw(1) }
            }
        }

        unsafe fn irq_guard_exit(_token: IrqGuardToken) {
            #[cfg(not(any(test, feature = "host-test")))]
            crate::guard::exit_irq("task runtime");
        }

        fn preempt_guard_enter() -> PreemptGuardToken {
            #[cfg(any(test, feature = "host-test"))]
            {
                // SAFETY: host-test mode models one balanced runtime preemption token.
                unsafe { PreemptGuardToken::from_raw(1) }
            }
            #[cfg(not(any(test, feature = "host-test")))]
            {
                match crate::guard::enter_lock_preempt() {
                    Some(token) => {
                        // SAFETY: the architecture owner identifies the live
                        // depth established by enter_lock_preempt.
                        unsafe { PreemptGuardToken::from_raw(token.into_raw()) }
                    }
                    None => PreemptGuardToken::NONE,
                }
            }
        }

        unsafe fn preempt_guard_exit(token: PreemptGuardToken) {
            assert!(
                !token.is_none(),
                "inherited owner scope passed to ordinary preemption exit"
            );
            #[cfg(not(any(test, feature = "host-test")))]
            {
                let token = unsafe { cpu_local::PreemptionToken::from_raw(token.into_raw()) }
                .expect("task preemption token must retain its architecture owner");
                crate::guard::exit_preempt(token);
            }
        }

        unsafe fn preempt_guard_exit_irq_return(token: PreemptGuardToken) {
            assert!(
                !token.is_none(),
                "inherited owner scope passed to IRQ-return preemption exit"
            );
            #[cfg(not(any(test, feature = "host-test")))]
            {
                let token = unsafe { cpu_local::PreemptionToken::from_raw(token.into_raw()) }
                .expect("IRQ-return token must retain its architecture owner");
                crate::guard::exit_preempt_from_irq_return(token);
            }
        }

        fn hardirq_enter() {
            crate::irq_time::enter();
        }

        fn hardirq_exit() {
            crate::irq_time::exit();
        }

        fn publish_local_scheduler_work() -> bool {
            #[cfg(any(test, feature = "host-test"))]
            {
                false
            }
            #[cfg(not(any(test, feature = "host-test")))]
            {
                crate::guard::publish_local_scheduler_work()
            }
        }

        fn finish_context_switch_tail() -> bool {
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
            #[cfg(any(test, feature = "host-test"))]
            {
                false
            }
            #[cfg(all(
                not(any(test, feature = "host-test")),
                feature = "irq"
            ))]
            {
                ax_hal::irq::in_irq_context()
            }
            #[cfg(all(
                not(any(test, feature = "host-test")),
                not(feature = "irq")
            ))]
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

        fn monotonic_now() -> ax_task::runtime::MonotonicInstant {
            ax_task::runtime::MonotonicInstant::from_nanos(
                ax_hal::time::monotonic_time_nanos(),
            )
            .expect("platform monotonic clock exceeded the signed ktime domain")
        }

        fn rq_clock_sample(cpu: RuntimeCpuId) -> ax_task::runtime::RqClockSample {
            let cpu_id = cpu.as_u32() as usize;
            // SAFETY: ax-task holds the target runqueue IRQ-save lock, which
            // pins the calling CPU for this complete remote-clock coupling.
            let clock_ns = unsafe { ax_hal::time::scheduler_clock_source(cpu_id) }
                .unwrap_or_else(|error| {
                    panic!("scheduler clock source for CPU {cpu_id} is unavailable: {error}")
                });
            ax_task::runtime::RqClockSample::new(
                ax_task::SchedulerTimestamp::from_nanos(clock_ns),
                crate::irq_time::total_for_cpu(cpu_id),
            )
        }

        fn publish_scheduler_deadline(update: ax_task::runtime::SchedulerDeadlineUpdate) {
            crate::clock_event_runtime::publish_local_scheduler_deadline(update);
        }

        fn idle_exit_restart_scheduler_tick() {
            #[cfg(all(feature = "irq", feature = "multitask"))]
            crate::clock_event_runtime::restart_current_scheduler_tick_after_idle(
                crate::clock_event_runtime::monotonic_now(),
            );
        }

        fn notify_scheduler_cpu(cpu: RuntimeCpuId) -> RuntimeStatus {
            #[cfg(any(feature = "ipi", feature = "wake-ipi"))]
            {
                let cpu_id = cpu.as_u32() as usize;
                if cpu_id >= ax_hal::cpu_num() {
                    return RuntimeStatus::InvalidArgument;
                }
                match ax_ipi::notify_cpu(ax_hal::irq::CpuId(cpu_id)) {
                    Ok(notification) => {
                        if notification == ax_ipi::IpiNotification::Sent {
                            #[cfg(feature = "qperf-metrics")]
                            record_scheduler_ipi_send();
                        }
                        RuntimeStatus::Success
                    }
                    Err(ax_hal::irq::IrqError::InvalidCpu) => RuntimeStatus::InvalidArgument,
                    Err(ax_hal::irq::IrqError::CpuOffline) => RuntimeStatus::NotInitialized,
                    Err(ax_hal::irq::IrqError::Busy) => RuntimeStatus::Busy,
                    Err(ax_hal::irq::IrqError::NoMemory) => RuntimeStatus::NoMemory,
                    Err(ax_hal::irq::IrqError::Unsupported) => RuntimeStatus::Unsupported,
                    Err(_) => RuntimeStatus::Platform,
                }
            }
            #[cfg(not(any(feature = "ipi", feature = "wake-ipi")))]
            {
                let _ = cpu;
                RuntimeStatus::Unsupported
            }
        }

        fn wait_for_interrupt() {
            // Linux keeps the idle task non-preemptible from do_idle() through
            // tick_nohz_idle_exit() and enters schedule_idle() only afterwards.
            // Keep the same ownership across the IRQ-enabled WFI window: an
            // interrupt may publish need-resched, but its return path cannot
            // switch away before this scope restores the stopped tick.
            let idle_exit_guard = crate::sync::PreemptGuard::new();
            ax_hal::asm::disable_irqs();
            unsafe {
                // SAFETY: local IRQs remain disabled through the immediately
                // following task-work and clockevent recheck, matching Linux
                // `current_clr_polling_and_test()`.
                ax_task::finish_current_cpu_idle_polling()
            }
            .expect("idle handoff requires an initialized current CPU");
            let mut now = crate::clock_event_runtime::monotonic_now();
            let mut needs_reschedule = ax_task::current_cpu_needs_resched()
                .expect("idle handoff requires an initialized current CPU");
            if needs_reschedule
                || crate::clock_event_runtime::local_clock_event_has_immediate_work(now)
            {
                crate::clock_event_runtime::restart_current_scheduler_tick_after_idle(now);
                ax_hal::asm::enable_irqs();
                drop(idle_exit_guard);
                return;
            }

            // Linux NOHZ removes the periodic scheduler tick only after idle
            // polling is withdrawn. Task deadlines remain selected by the
            // same physical clockevent and are reprogrammed in this IRQ-off
            // transaction.
            crate::clock_event_runtime::stop_current_scheduler_tick_for_idle();
            now = crate::clock_event_runtime::monotonic_now();
            needs_reschedule = ax_task::current_cpu_needs_resched()
                .expect("idle handoff requires an initialized current CPU");
            if needs_reschedule
                || crate::clock_event_runtime::local_clock_event_has_immediate_work(now)
            {
                crate::clock_event_runtime::restart_current_scheduler_tick_after_idle(now);
                ax_hal::asm::enable_irqs();
                drop(idle_exit_guard);
                return;
            }

            ax_hal::asm::wait_for_irqs_disabled();

            // A non-scheduling interrupt may leave the CPU in the idle loop,
            // in which case the tick stays stopped just as in Linux do_idle().
            // Work that makes the idle thread yield restarts the tick before
            // the scheduler can select a non-idle thread.
            let irq_guard = crate::sync::IrqSaveGuard::new();
            now = crate::clock_event_runtime::monotonic_now();
            needs_reschedule = ax_task::current_cpu_needs_resched()
                .expect("idle wake requires an initialized current CPU");
            if needs_reschedule
                || crate::clock_event_runtime::local_clock_event_has_immediate_work(now)
            {
                crate::clock_event_runtime::restart_current_scheduler_tick_after_idle(now);
            }
            drop(irq_guard);
            drop(idle_exit_guard);
        }

        fn allocate_stack(_request: StackRequest) -> RuntimeHandleResult {
            match allocate_runtime_stack(_request) {
                Ok(handle) => RuntimeHandleResult::success(handle.into_raw()),
                Err(status) => RuntimeHandleResult::failure(status),
            }
        }

        fn deallocate_stack(_stack: StackHandle) {
            assert_eq!(
                deallocate_runtime_stack(_stack),
                RuntimeStatus::Success,
                "reclaimable task stack destruction failed"
            );
        }

        fn allocate_tls(_request: TlsRequest) -> RuntimeHandleResult {
            allocate_runtime_tls(_request)
        }

        fn deallocate_tls(_tls: TlsHandle) {
            assert_eq!(
                deallocate_runtime_tls(_tls),
                RuntimeStatus::Success,
                "reclaimable task TLS destruction failed"
            );
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

        fn destroy_context(_context: ExecutionContextHandle) {
            assert_eq!(
                destroy_runtime_context(_context),
                RuntimeStatus::Success,
                "task context remained live after scheduler switch tail"
            );
        }

        fn destroy_address_space(
            address_space: AddressSpaceHandle,
        ) -> AddressSpaceDestroyOutcome {
            destroy_runtime_address_space(address_space)
        }

        fn arm_address_space_reclaim(
            address_space: AddressSpaceHandle,
        ) -> AddressSpaceReclaimArmOutcome {
            arm_runtime_address_space_reclaim(address_space)
        }

        fn address_space_membarrier_state(
            address_space: AddressSpaceHandle,
        ) -> AddressSpaceMembarrierState {
            runtime_address_space_membarrier_state(address_space)
        }

        fn update_address_space_membarrier_state(
            address_space: AddressSpaceHandle,
            registration: MembarrierRegistration,
            phase: MembarrierRegistrationPhase,
        ) -> AddressSpaceMembarrierState {
            update_runtime_address_space_membarrier_state(address_space, registration, phase)
        }

        fn synchronize_membarrier_cpu(
            cpu: RuntimeCpuId,
            action: RuntimeMembarrierAction,
        ) -> RuntimeStatus {
            let cpu = cpu.as_u32() as usize;
            let callback = match action {
                RuntimeMembarrierAction::MemoryBarrier => membarrier_ipi_memory_barrier,
                RuntimeMembarrierAction::RefreshRunQueue => {
                    membarrier_ipi_refresh_run_queue
                }
            };
            #[cfg(all(feature = "irq", feature = "ipi"))]
            {
                // SAFETY: both callbacks are fixed, allocation-free hard-IRQ
                // operations and carry no argument lifetime.
                match unsafe {
                    crate::ipi_delivery::run_on_cpu_sync(cpu, callback, core::ptr::null_mut())
                } {
                    Ok(()) => RuntimeStatus::Success,
                    Err(ax_hal::irq::IrqError::CpuOffline) => RuntimeStatus::Busy,
                    Err(ax_hal::irq::IrqError::InvalidCpu) => RuntimeStatus::InvalidArgument,
                    Err(_) => RuntimeStatus::Platform,
                }
            }
            #[cfg(not(all(feature = "irq", feature = "ipi")))]
            {
                // A UP runtime executes the same hard-call ABI locally. SMP
                // membarrier requires the explicit `ipi` capability.
                let current = unsafe { Self::current_cpu_id() }.as_u32() as usize;
                if cpu != current {
                    return RuntimeStatus::Unsupported;
                }
                // SAFETY: the selected fixed callback takes no argument.
                unsafe { callback(core::ptr::null_mut()) };
                RuntimeStatus::Success
            }
        }

        unsafe fn switch_context(switch: ContextSwitch) {
            // SAFETY: the TaskRuntime contract passes one committed move-only
            // switch transaction under the active scheduler baton.
            unsafe { switch_runtime_context(switch) };
        }

        fn activate_address_space(activation: AddressSpaceActivation) -> RuntimeStatus {
            activate_runtime_address_space(activation)
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

        fn emergency_console_write(message: &str) {
            ax_hal::console::write_bytes(message.as_bytes());
        }

        fn fatal_invariant(code: u32, argument: usize) -> ! {
            panic!("ax-task invariant {code} failed with argument {argument:#x}")
        }
    }
}
