//! Operating-system capability table consumed by ax-task.

use trait_ffi::def_extern_trait;

use super::*;

/// OS capabilities needed by the scheduling core.
///
/// Implementations must keep task-system and CPU-local handles valid until
/// shutdown. All IRQ-path methods must be allocation-free and non-blocking.
#[def_extern_trait(mod_path = "runtime", abi = "rust")]
pub trait TaskRuntime {
    /// Returns the runtime-owned task-system handle, or `NONE` before setup.
    ///
    /// # Safety
    ///
    /// A non-`NONE` result must identify a pinned [`crate::TaskSystem`] that
    /// remains live until shutdown. The linked runtime provider is the trust
    /// root for this raw handle; callers cannot validate it dynamically.
    unsafe fn task_system_handle() -> TaskSystemHandle;

    /// Captures the complete pinned scheduler capability for the calling CPU.
    ///
    /// This is a CPU-owned capability, not a migration-stable task handle. The
    /// caller must retain an IRQ guard or scheduler-frame baton from before this
    /// query until every dereference of the returned object has completed.
    ///
    /// # Safety
    ///
    /// The returned identity, owner-only [`crate::CpuLocal`] handle and
    /// Arc-backed [`crate::CpuRemote`] handle must all describe the same calling
    /// CPU and remain live until shutdown. The local address must originate
    /// from the allocation's mutable owner capability, not from a shared
    /// `CpuLocal` borrow. Before reconstructing a reference, the caller must
    /// claim the returned remote endpoint's owner gate and retain both that
    /// claim and its CPU pin for the complete derived-borrow lifetime.
    unsafe fn current_cpu_owner_handles() -> CurrentCpuOwnerHandles;

    /// Returns the Arc-backed [`crate::CpuRemote`] endpoint for the calling CPU.
    ///
    /// This is the scheduler-adjacent current-CPU fast path. Unlike
    /// [`Self::cpu_remote_handle`], it must not derive a CPU identifier and
    /// resolve that identifier through the global task-system registry.
    ///
    /// # Safety
    ///
    /// The caller must prevent migration until it has finished every read
    /// through the returned endpoint. A non-`NONE` result must identify the
    /// calling CPU's Arc-backed [`crate::CpuRemote`] and remain live until
    /// shutdown. It must not identify a [`crate::CpuLocal`] or any other
    /// allocation.
    unsafe fn current_cpu_remote_handle() -> CpuRemoteHandle;

    /// Returns the generation-bearing scheduler identity bound to the calling
    /// execution context.
    ///
    /// This is the identity-only equivalent of Linux's direct `current`
    /// pointer for fast paths that do not need an owner reference. Providers
    /// must read the task-owned runtime context selected by the architecture
    /// current-thread register. [`ThreadIdentityV1::NONE`] denotes an unbound
    /// bootstrap context.
    ///
    /// A bound result must equal the identity in
    /// [`Self::current_thread_publication`] and remain immutable for the
    /// complete lifetime of that runtime context.
    fn current_thread_identity() -> ThreadIdentityV1;

    /// Returns the scheduler publication bound to the calling execution context.
    ///
    /// This is the local equivalent of Linux's direct `current` task pointer.
    /// Providers must read the task-owned runtime context selected by the
    /// architecture current-thread register; they must not resolve the local
    /// publication through a remote runqueue endpoint.
    /// [`CurrentThreadPublication::NONE`] denotes an unbound bootstrap context.
    ///
    /// A bound result must match the scheduler core retained by the current
    /// task and remain immutable for the complete lifetime of that runtime
    /// context. Preemption and migration must not change this task identity.
    fn current_thread_publication() -> CurrentThreadPublication;

    /// Tests the current execution context's advisory preemption-pending state.
    ///
    /// This is the runtime equivalent of Linux's `need_resched()` safe-point
    /// query. It must read the architecture-selected current state without
    /// disabling preemption or claiming scheduler work. A `false` result is
    /// only a snapshot.
    fn current_preemption_pending() -> bool;

    /// Returns the Arc-backed [`crate::CpuRemote`] endpoint for `cpu`.
    ///
    /// Unlike [`Self::current_cpu_local_handle`], this handle must never point
    /// at [`crate::CpuLocal`]. Remote producers may retain and dereference the
    /// endpoint without aliasing the owner CPU's mutable runqueue borrow.
    ///
    /// # Safety
    ///
    /// A non-`NONE` result must identify the Arc-backed [`crate::CpuRemote`]
    /// endpoint for `cpu` and remain live until shutdown. It must not identify
    /// a [`crate::CpuLocal`] or any other allocation.
    unsafe fn cpu_remote_handle(cpu: RuntimeCpuId) -> CpuRemoteHandle;

    /// Returns the calling CPU's logical identifier under an existing pin.
    ///
    /// # Safety
    ///
    /// The caller must prevent migration until it has finished the local
    /// operation associated with the returned identity.
    unsafe fn current_cpu_id() -> RuntimeCpuId;

    /// Prepares one owner CPU's runtime facilities for scheduler publication.
    ///
    /// The caller holds local IRQ exclusion and has validated that the CPU is
    /// currently offline. The implementation must prepare every CPU-local
    /// wake source needed by the scheduler, including its physical
    /// clockevent, before returning success. It must not allocate, block,
    /// invoke callbacks, or re-enter ax-task. Failure must leave the runtime
    /// offline and retryable.
    fn prepare_cpu_online(cpu: RuntimeCpuId) -> RuntimeStatus;

    /// Stops one owner CPU's runtime facilities before final offline publication.
    ///
    /// The scheduler has already closed remote admission and proved the CPU
    /// quiescent, but still reports it online while this hook runs. The
    /// implementation must stop every CPU-local wake source, including its
    /// physical clockevent, before returning success. Failure must leave the
    /// runtime retryable. The hook must not allocate, block, invoke callbacks,
    /// or re-enter ax-task.
    fn prepare_cpu_offline(cpu: RuntimeCpuId) -> RuntimeStatus;

    /// Saves the raw local-interrupt state and disables local interrupts.
    ///
    /// This operation does not enter the scheduler's nested IRQ-guard owner
    /// scope. Synchronization guards need that narrower capability so an IRQ
    /// return can still own and consume its explicit preemption depth.
    fn local_irq_save_and_disable() -> LocalIrqState;

    /// Restores a raw local-interrupt state.
    ///
    /// # Safety
    ///
    /// `state` must have been returned by
    /// [`Self::local_irq_save_and_disable`] on this CPU and must be restored
    /// exactly once in properly nested order.
    unsafe fn local_irq_restore(state: LocalIrqState);

    /// Saves raw interrupt state, disables local IRQs and enters nested guards.
    fn irq_guard_enter() -> IrqGuardToken;

    /// Leaves one nested IRQ guard and restores the outer raw state if needed.
    ///
    /// # Safety
    ///
    /// `token` must have been returned by `irq_guard_enter` on this CPU and
    /// must be exited exactly once. Tokens may be exited in non-LIFO order.
    unsafe fn irq_guard_exit(token: IrqGuardToken);

    /// Prevents the current task context from being preempted or migrated.
    ///
    /// This capability does not disable hardware interrupts. It is valid only
    /// in task context or inside an active scheduler frame; scheduler state
    /// shared with hard-IRQ producers uses the separate IRQ-safe lock domain.
    ///
    /// When an enclosing scheduler frame or runtime IRQ guard already owns the
    /// CPU for the complete lock scope, the runtime returns
    /// [`PreemptGuardToken::NONE`]. The matching lock guard then releases only
    /// its raw lock; it must not manufacture another ordinary preemption depth
    /// inside the existing owner transaction.
    fn preempt_guard_enter() -> PreemptGuardToken;

    /// Leaves one nested task-preemption guard.
    ///
    /// The final exit may enter the scheduler when work is pending. When the
    /// caller already owns a scheduler frame, it must consume only this nested
    /// depth and preserve the scheduler baton and raw IRQ state.
    ///
    /// # Safety
    ///
    /// A non-`NONE` `token` must have been returned by
    /// `preempt_guard_enter` on this task execution context and must be exited
    /// exactly once. Tokens may be exited in non-LIFO order.
    unsafe fn preempt_guard_exit(token: PreemptGuardToken);

    /// Leaves one nested task-preemption guard at a hard-IRQ return boundary.
    ///
    /// Unlike [`Self::preempt_guard_exit`], the final exit may enter the
    /// scheduler while hardware IRQs remain disabled and must return with IRQs
    /// disabled for the architecture exception epilogue.
    ///
    /// # Safety
    ///
    /// A non-`NONE` `token` must have been returned by
    /// [`Self::preempt_guard_enter`] on this task execution context and must be
    /// exited exactly once.
    unsafe fn preempt_guard_exit_irq_return(token: PreemptGuardToken);

    /// Publishes entry into the runtime's hard-interrupt lifecycle.
    fn hardirq_enter();

    /// Publishes exit from the runtime's hard-interrupt lifecycle.
    fn hardirq_exit();

    /// Publishes sticky scheduler work to the current CPU's architecture
    /// preemption state and reports whether a local safe point makes a self-IPI
    /// unnecessary.
    ///
    /// The caller owns an IRQ guard and has already published the scheduler
    /// payload. Before returning, the runtime must set the architecture-owned
    /// `need_resched` state observed by preemption and IRQ return. It may return
    /// `true` only when that state is guaranteed to reach the scheduler before
    /// the CPU can sleep: through hard-IRQ return, an active scheduler/preemption
    /// guard, or atomic conversion of the final task-context IRQ guard into a
    /// scheduler baton.
    fn publish_local_scheduler_work() -> bool;

    /// Withdraws the outgoing runtime context's CPU binding after raw switch.
    ///
    /// The incoming context calls this exactly once while local IRQs remain
    /// disabled and before the scheduler clears the outgoing thread's
    /// `on_cpu` publication. The implementation must not allocate, block,
    /// invoke callbacks, consume the scheduler baton, or re-enter ax-task. It
    /// returns the deferred resource-release edge and the post-switch
    /// monotonic clock sample used for incoming CPU-time accounting. Sampling
    /// after the incoming context binding matches Linux `vtime_task_switch()`.
    /// Any failure is an unrecoverable runtime invariant: the raw switch has
    /// already committed, so there is no compatibility retry path.
    fn finish_context_switch_tail() -> (bool, u64);

    /// Consumes the CPU-local scheduler switch baton on a fresh context.
    ///
    /// The baton is not an [`IrqGuardToken`] and never belongs to a task. A
    /// resumed scheduler frame consumes the current CPU's baton after the raw
    /// switch returns; a fresh trampoline calls this hook exactly once after
    /// completing the switch tail.
    fn finish_initial_context_switch();

    /// Enters the current CPU's exact scheduler switch phase.
    ///
    /// The runtime validates `entry`, disables hardware IRQs, and atomically
    /// creates one CPU-local baton. For [`RuntimeSchedulerEntry::PreemptExit`]
    /// and [`RuntimeSchedulerEntry::IrqReturn`], it must transform the exact
    /// final lock-preemption depth into the scheduler depth. For
    /// [`RuntimeSchedulerEntry::IrqGuardExit`], it must instead transform the
    /// final task-context IRQ-publication depth. Neither path may expose a
    /// fully preemptible intermediate state. The runtime must not save this
    /// phase in an execution context or migrate ordinary IRQ tokens with tasks.
    /// [`RuntimeSchedulerEntry::IrqReturnContinuation`] must reproduce Linux's
    /// IRQ-return pass boundary: establish one preemption depth, enable local
    /// IRQs without a live scheduler baton, disable them again, then convert
    /// that exact depth into the next scheduler baton.
    fn scheduler_frame_guard_enter(
        origin: RuntimeScheduleOrigin,
        entry: RuntimeSchedulerEntry,
    ) -> RuntimeStatus;

    /// Consumes the current CPU's scheduler switch baton after switch tail.
    ///
    /// This hook restores task-context hardware IRQ state and must not schedule
    /// recursively. It returns `true` only when deferred callbacks may run with
    /// IRQs enabled and every ordinary guard clear.
    fn scheduler_frame_guard_exit(return_to: RuntimeSchedulerReturn) -> bool;

    /// Returns whether execution is currently inside a hard interrupt.
    fn in_hard_irq() -> bool;

    /// Validates an entry before it publishes task state or creates a baton.
    ///
    /// This is the runtime equivalent of Linux `might_sleep()` plus the final
    /// scheduler-entry context check. It must return [`RuntimeStatus::UnsafeContext`]
    /// while any ordinary IRQ/preemption guard is live or hardware execution is
    /// still in hard IRQ context.
    fn validate_schedule_context(origin: RuntimeScheduleOrigin) -> RuntimeStatus;

    /// Validates one owner-CPU scheduler-state access.
    ///
    /// This is the runtime equivalent of Linux's `lockdep_assert_rq_held()`.
    /// It must return [`RuntimeStatus::Success`] only while the current CPU is
    /// pinned by an ordinary IRQ guard or owns an active scheduler baton.
    /// Unlike [`Self::validate_schedule_context`], a completely unguarded task
    /// context is invalid here: an interrupt-return scheduler entry could
    /// otherwise re-enter over a live mutable [`crate::CpuLocal`] borrow.
    fn validate_owner_cpu_context() -> RuntimeStatus;

    /// Returns one sample from the finite monotonic `ktime` domain.
    fn monotonic_now() -> MonotonicInstant;

    /// Returns one coherent source sample for `cpu`'s runqueue clocks.
    ///
    /// Linux calls `sched_clock_cpu(cpu_of(rq))` while holding the target
    /// runqueue lock, including direct remote wakeups. The runtime may derive
    /// every CPU source from one synchronized hardware counter, but it must not
    /// silently substitute the calling CPU when per-CPU sources differ.
    /// Scheduler absolute values must never be compared directly with
    /// monotonic deadlines.
    fn rq_clock_sample(cpu: RuntimeCpuId) -> RqClockSample;

    /// Commits the current CPU's complete scheduler-deadline state.
    ///
    /// The runtime owns the physical clockevent. It must ignore generations
    /// older than the most recently accepted update and merge the accepted
    /// scheduler deadline with non-scheduler sources before programming hardware. This
    /// hook is callable from ordinary task context, so the runtime must hold
    /// local IRQ exclusion across both state publication and hardware
    /// programming instead of relying on an implicit caller-side guard.
    ///
    /// This is an infallible ownership boundary, like Linux's hrtimer-to-
    /// clockevent rearm path. A runtime must absorb an expired hardware
    /// deadline by clamping it to the device's minimum nonzero delta and treat a device that cannot
    /// retain a wakeup source as a runtime-fatal invariant. Returning a
    /// recoverable error here would leave the scheduler queue and physical
    /// clockevent in an unknowable half-committed state.
    fn publish_scheduler_deadline(update: SchedulerDeadlineUpdate);

    /// Notifies `cpu` after the scheduler has published owner work.
    ///
    /// Sticky scheduler flags and owner inbox membership remain owned by
    /// ax-task. The runtime transports only a coalescible physical edge,
    /// matching Linux's split between `TIF_NEED_RESCHED`/wake-list state and
    /// the reschedule or call-function IPI. Success
    /// means either a fresh edge was sent or an in-flight edge already covers
    /// this publication; every other status is an unrecoverable lifecycle
    /// violation.
    fn notify_scheduler_cpu(cpu: RuntimeCpuId) -> RuntimeStatus;

    /// Restarts the periodic scheduler tick while leaving the idle thread.
    ///
    /// Mirrors Linux `tick_nohz_idle_exit()` before `schedule_idle()`: the idle
    /// loop's own IRQ-off checkpoints cannot cover a reschedule request that
    /// becomes visible only after IRQs are re-enabled, so the owner schedule
    /// that switches away from the idle thread owns the restart. The caller is
    /// the owner CPU inside its scheduler frame with local IRQs disabled. The
    /// implementation must not allocate, block, invoke callbacks, or re-enter
    /// ax-task; an already running tick is a no-op.
    fn idle_exit_restart_scheduler_tick();

    /// Commits one local interrupt wait after the scheduler publishes polling.
    ///
    /// The implementation must disable local interrupts, call
    /// `finish_current_cpu_idle_polling`, and immediately recheck sticky task
    /// work and physical clockevent state before stopping the periodic tick or
    /// sleeping. It may use the architecture's atomic IRQ-enable-and-wait
    /// primitive only when all sources remain idle. Task deadlines stay armed
    /// while the scheduler tick is stopped. Work published before polling was
    /// cleared is observed by the final recheck; work published afterwards
    /// owns a physical interrupt edge. The tick must restart before runnable
    /// work can leave the idle loop, but may remain stopped across
    /// non-scheduling IRQs.
    fn wait_for_interrupt();

    /// Allocates a guarded stack satisfying `request`.
    ///
    /// On success, `handle` must be non-zero and uniquely identify a live stack
    /// accepted by [`Self::deallocate_stack`] until ownership is transferred.
    fn allocate_stack(request: StackRequest) -> RuntimeHandleResult;

    /// Releases a stack after the reaper proves no context can reference it.
    ///
    /// Ownership transfers exactly once. The scheduler has already crossed the
    /// switch-tail lifetime boundary, so a provider must treat an inability to
    /// release this handle as a fatal runtime invariant rather than inventing a
    /// polling retry protocol.
    fn deallocate_stack(stack: StackHandle);

    /// Allocates a TLS area satisfying `request`.
    ///
    /// On success, `handle` must be non-zero and uniquely identify a live TLS
    /// allocation accepted by [`Self::deallocate_tls`] until ownership moves.
    fn allocate_tls(request: TlsRequest) -> RuntimeHandleResult;

    /// Releases a TLS area after its execution context has been destroyed.
    ///
    /// Ownership transfers exactly once after no context can reference the
    /// allocation. Failure is a fatal runtime invariant.
    fn deallocate_tls(tls: TlsHandle);

    /// Creates a kernel execution context.
    ///
    /// On success, `handle` must be non-zero and uniquely identify a live
    /// context accepted by [`Self::destroy_context`].
    fn create_kernel_context(request: KernelContextRequest) -> RuntimeHandleResult;

    /// Creates a user-capable execution context with a mandatory address space.
    ///
    /// On success, `handle` must follow the same ownership contract as
    /// [`Self::create_kernel_context`].
    fn create_user_context(request: UserContextRequest) -> RuntimeHandleResult;

    /// Binds a created context to its final generation-bearing thread ID.
    ///
    /// The runtime must validate the context handle and install the association
    /// atomically. A failed call must leave the context unbound so construction
    /// can destroy it. This hook runs under the task registry's preempt-only
    /// lock; it must not allocate, block, invoke callbacks, or re-enter
    /// ax-task.
    ///
    /// Providers without execution contexts still export this capability and
    /// return `Unsupported`, keeping trait-FFI symbol completeness explicit.
    fn bind_context_thread(binding: ContextThreadBinding) -> RuntimeStatus;

    /// Destroys an execution context that cannot be scheduled again.
    ///
    /// The registry makes a record reclaimable only after switch tail clears
    /// physical CPU ownership. Destruction is therefore a single, infallible
    /// ownership transfer; a provider must report an impossible live-context
    /// state through its own fatal invariant path rather than return `Busy`.
    fn destroy_context(context: ExecutionContextHandle);

    /// Releases an address-space object after no CPU retains it as active mm.
    ///
    /// [`AddressSpaceDestroyOutcome::Active`] leaves the object live and
    /// accepted by a later retry. This task-context operation may drop the OS
    /// ownership lease; it is never invoked from the IRQ-off context-switch
    /// path. The runtime must treat an invalid handle as a fatal provider
    /// invariant rather than expose a compatibility status.
    fn destroy_address_space(address_space: AddressSpaceHandle) -> AddressSpaceDestroyOutcome;

    /// Arms one deferred retry after destruction observed an active CPU lease.
    ///
    /// The token is already queued in ax-task before this call. The runtime
    /// returns [`AddressSpaceReclaimArmOutcome::Ready`] if no CPU lease remains,
    /// or records an allocation-free notification obligation and returns
    /// [`AddressSpaceReclaimArmOutcome::Armed`]. The CPU that drops the last
    /// lease must then call [`crate::notify_address_space_reclaim`]. An invalid
    /// handle is a fatal provider invariant.
    fn arm_address_space_reclaim(
        address_space: AddressSpaceHandle,
    ) -> AddressSpaceReclaimArmOutcome;

    /// Loads the shared `mm` identity and membarrier registration state.
    ///
    /// This operation runs while rq locks or local IRQ exclusion may be held.
    /// It must be a fixed, allocation-free atomic lookup and must not acquire
    /// an OS lock or re-enter ax-task. An invalid handle is a fatal provider
    /// invariant.
    fn address_space_membarrier_state(
        address_space: AddressSpaceHandle,
    ) -> AddressSpaceMembarrierState;

    /// Advances one irreversible per-`mm` membarrier registration phase.
    ///
    /// `Begin` publishes the requested bit before ax-task inspects runqueues;
    /// `Complete` publishes the ready bit only after synchronous target-rq
    /// refresh. The operation must be allocation-free and atomic.
    fn update_address_space_membarrier_state(
        address_space: AddressSpaceHandle,
        registration: MembarrierRegistration,
        phase: MembarrierRegistrationPhase,
    ) -> AddressSpaceMembarrierState;

    /// Executes one bounded membarrier action synchronously on `cpu`.
    ///
    /// Providers must not return success until the target callback completes.
    /// The remote callback runs in hard-IRQ context and may only execute the
    /// selected full barrier or the ax-task rq refresh entry; it must not
    /// allocate, sleep, or invoke arbitrary OS callbacks.
    fn synchronize_membarrier_cpu(
        cpu: RuntimeCpuId,
        action: RuntimeMembarrierAction,
    ) -> RuntimeStatus;

    /// Consumes one committed scheduler-switch transaction with local
    /// interrupts disabled.
    ///
    /// # Safety
    ///
    /// Both execution-context handles and every non-empty address-space handle
    /// in `plan` must identify live runtime objects. The caller must have
    /// committed scheduler state and released runqueue locks. The provider
    /// must validate and prepare both transitions before committing either one,
    /// then consume the plan exactly once.
    unsafe fn switch_context(plan: RuntimeSwitchPlan);

    /// Flushes the current address space's local translation cache.
    fn flush_tlb_local(start: usize, size: usize);

    /// Emits an allocation-free context-switch trace record.
    fn trace_sched_switch(record: SchedSwitchRecord);

    /// Writes directly to the runtime's emergency console without taking an
    /// OS lock or re-entering the scheduler.
    fn emergency_console_write(message: &str);

    /// Reports an unrecoverable scheduler invariant and terminates execution.
    fn fatal_invariant(code: u32, argument: usize) -> !;
}
