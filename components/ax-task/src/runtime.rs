//! Operating-system capability boundary used by the scheduler.
//!
//! The boundary deliberately passes only scalar values, function pointers and
//! transparent opaque handles. Runtime resources remain owned by explicit,
//! move-only tokens held by the scheduler until task-context reclamation.

use trait_ffi::def_extern_trait;

macro_rules! opaque_handle {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
        #[repr(transparent)]
        pub struct $name(usize);

        impl $name {
            /// Sentinel returned before the corresponding runtime object exists.
            pub const NONE: Self = Self(0);

            /// Creates a handle from the runtime-owned opaque value.
            ///
            /// # Safety
            ///
            /// A non-zero `raw` value must identify a live runtime-owned object
            /// or resource of this exact handle type. The caller must uphold all
            /// provenance, lifetime, pinning, aliasing, and ownership invariants
            /// required by operations that consume or dereference the handle.
            /// Use [`Self::NONE`] for the absent-handle sentinel.
            #[doc = concat!(
                "\n```compile_fail\n",
                "use ax_task::runtime::", stringify!($name), ";\n",
                "let _handle = ", stringify!($name), "::from_raw(1);\n",
                "```"
            )]
            pub const unsafe fn from_raw(raw: usize) -> Self {
                Self(raw)
            }

            /// Returns the runtime-owned opaque value.
            pub const fn into_raw(self) -> usize {
                self.0
            }

            /// Returns whether this is the absent-handle sentinel.
            pub const fn is_none(self) -> bool {
                self.0 == 0
            }
        }
    };
}

opaque_handle!(
    /// Opaque pointer-sized handle to the runtime-owned task system.
    TaskSystemHandle
);
opaque_handle!(
    /// Opaque address of the current CPU's pinned owner-only scheduler object.
    ///
    /// Consumers must claim the corresponding [`crate::CpuRemote`] owner gate
    /// before reconstructing any reference from this address.
    CurrentCpuLocalHandle
);
opaque_handle!(
    /// Opaque pointer-sized handle to one Arc-backed remote CPU endpoint.
    ///
    /// Remote and owner-only CPU handles are intentionally not interchangeable:
    ///
    /// ```compile_fail
    /// use ax_task::runtime::{CpuRemoteHandle, CurrentCpuLocalHandle};
    ///
    /// fn borrow_owner(_handle: CurrentCpuLocalHandle) {}
    /// borrow_owner(CpuRemoteHandle::NONE);
    /// ```
    CpuRemoteHandle
);
opaque_handle!(
    /// Opaque handle to an architecture execution context.
    ExecutionContextHandle
);
opaque_handle!(
    /// Opaque handle to a runtime-owned stack allocation.
    StackHandle
);
opaque_handle!(
    /// Opaque handle to a runtime-owned TLS allocation.
    TlsHandle
);
opaque_handle!(
    /// Borrowed opaque handle to a runtime-owned address space.
    AddressSpaceHandle
);

/// Address-space state selected for the next scheduler context.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum AddressSpaceActivationKind {
    /// A kernel thread borrows the CPU's current active address space.
    KernelLazy = 0,
    /// A user thread activates the supplied runtime-owned address space.
    User       = 1,
}

/// Explicit address-space transaction passed to the runtime switch boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct AddressSpaceActivation {
    kind: AddressSpaceActivationKind,
    address_space: AddressSpaceHandle,
}

impl AddressSpaceActivation {
    /// Enters the architecture's current lazy kernel address-space state.
    pub const KERNEL_LAZY: Self = Self {
        kind: AddressSpaceActivationKind::KernelLazy,
        address_space: AddressSpaceHandle::NONE,
    };

    /// Selects one user address space.
    pub const fn user(address_space: AddressSpaceHandle) -> Self {
        assert!(
            !address_space.is_none(),
            "a user activation requires a runtime address-space handle"
        );
        Self {
            kind: AddressSpaceActivationKind::User,
            address_space,
        }
    }

    /// Derives the scheduler activation from a thread's nullable `mm` handle.
    pub const fn for_thread(address_space: AddressSpaceHandle) -> Self {
        if address_space.is_none() {
            Self::KERNEL_LAZY
        } else {
            Self::user(address_space)
        }
    }

    /// Returns the requested activation kind.
    pub const fn kind(self) -> AddressSpaceActivationKind {
        self.kind
    }

    /// Returns the user handle, if this activates a user address space.
    pub const fn user_handle(self) -> Option<AddressSpaceHandle> {
        match self.kind {
            AddressSpaceActivationKind::KernelLazy => None,
            AddressSpaceActivationKind::User => Some(self.address_space),
        }
    }
}

/// Unique destruction right for one runtime-owned address-space object.
///
/// The scheduler may copy [`AddressSpaceHandle`] values derived from this
/// token into dispatch metadata, but exactly one token owns the eventual
/// [`TaskRuntime::destroy_address_space`] operation.
#[repr(transparent)]
#[derive(Debug, Eq, PartialEq)]
pub struct AddressSpaceToken(usize);

impl AddressSpaceToken {
    /// Empty token used by kernel threads and pure scheduler models.
    pub const NONE: Self = Self(0);

    /// Creates an owning token from a fresh runtime object.
    ///
    /// # Safety
    ///
    /// A non-zero value must identify a live runtime-owned address-space
    /// object whose unique destruction right is transferred to the caller.
    pub const unsafe fn from_raw(raw: usize) -> Self {
        Self(raw)
    }

    /// Borrows the opaque identity without transferring destruction rights.
    pub const fn handle(&self) -> AddressSpaceHandle {
        // SAFETY: a live owning token keeps the same runtime object alive for
        // the duration of the returned scalar borrow.
        unsafe { AddressSpaceHandle::from_raw(self.0) }
    }

    /// Returns whether this token owns no runtime object.
    pub const fn is_none(&self) -> bool {
        self.0 == 0
    }
}

/// Result of consuming an address-space destruction attempt.
///
/// The runtime accepts only a live handle derived from the matching
/// [`AddressSpaceToken`]. A stale or malformed handle is an unrecoverable
/// provider invariant and is not represented here.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum AddressSpaceDestroyOutcome {
    /// No CPU retains the address space and the runtime consumed its object.
    Released = 0,
    /// At least one CPU still retains the address space as its active mm.
    Active   = 1,
}

/// Result of arming the active-mm last-user notification.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum AddressSpaceReclaimArmOutcome {
    /// No CPU lease remains; the scheduler must retry destruction now.
    Ready = 0,
    /// The runtime will publish a readiness edge when the last lease leaves.
    Armed = 1,
}
opaque_handle!(
    /// Token returned by the nested IRQ guard service.
    IrqGuardToken
);
opaque_handle!(
    /// Token returned by the nested task-preemption guard service.
    PreemptGuardToken
);

/// Logical CPU identifier exchanged with the operating-system runtime.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct RuntimeCpuId(u32);

impl RuntimeCpuId {
    /// Creates a logical CPU identifier.
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    /// Returns the numeric logical CPU identifier.
    pub const fn as_u32(self) -> u32 {
        self.0
    }
}

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

/// Scheduler entry whose context constraints the runtime must validate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum RuntimeScheduleOrigin {
    /// A thread is about to publish or commit a blocking state.
    Block   = 0,
    /// A thread voluntarily yields its remaining service.
    Yield   = 1,
    /// A thread permanently exits.
    Exit    = 2,
    /// A sticky preemption request is serviced from task context.
    Preempt = 3,
}

/// Typed source of one scheduler-frame baton.
///
/// The runtime uses this value to validate and atomically transform its
/// CPU-local preemption state. In particular, preemption-guard exits retain
/// their final lock depth until the scheduler frame owns the baton, closing the
/// interrupt window between enabling preemption and entering the scheduler.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum RuntimeSchedulerEntry {
    /// Ordinary task context with IRQs enabled and no preemption guard.
    Task         = 0,
    /// Final task-context preemption guard exit with IRQs disabled.
    ///
    /// The runtime retains the final preemption depth while it disables raw
    /// IRQs, then atomically converts that depth into the scheduler baton.
    PreemptExit  = 1,
    /// Final IRQ-return preemption guard exit with IRQs still disabled.
    IrqReturn    = 2,
    /// Final task-context IRQ publication guard exit with IRQs disabled.
    ///
    /// The runtime retains the final IRQ-guard depth after publishing local
    /// scheduler work, then atomically converts that depth into the scheduler
    /// baton. This is the local counterpart of a remote scheduler IPI.
    IrqGuardExit = 3,
}

/// Raw IRQ state expected by the suspended scheduler continuation.
///
/// This is continuation-local rather than CPU-local: a context resumed by an
/// IRQ-return schedule may itself have been suspended in an ordinary task
/// schedule, and vice versa.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum RuntimeSchedulerReturn {
    /// Resume ordinary task context with local IRQs enabled.
    Task      = 0,
    /// Resume the architecture trap epilogue with local IRQs disabled.
    IrqReturn = 1,
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

/// Stack allocation requirements supplied to the runtime.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct StackRequest {
    /// Usable stack bytes, excluding the guard region.
    pub usable_size: usize,
    /// Required stack alignment in bytes.
    pub alignment: usize,
    /// Number of inaccessible guard bytes below the usable range.
    pub guard_size: usize,
}

/// Kernel context entry point.
///
/// Per-thread arguments remain in scheduler-owned thread metadata and are
/// recovered by the entry trampoline through the current-thread facade. This
/// matches the four architecture `TaskContext::init` contracts, which enter a
/// fresh context without a portable argument register contract.
pub type KernelEntry = unsafe extern "C" fn() -> !;

/// Architecture-neutral request for a new kernel execution context.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct KernelContextRequest {
    /// Runtime-owned stack backing the context.
    pub stack: StackHandle,
    /// Initial instruction entry point.
    pub entry: KernelEntry,
    /// Optional TLS allocation.
    pub tls: TlsHandle,
}

/// Architecture-neutral request for a context that will enter userspace.
///
/// The initial entry is still a trusted runtime trampoline. Address-space
/// ownership and activation are scheduler resources, not register-context
/// construction inputs.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct UserContextRequest {
    /// Runtime-owned stack backing the trusted entry trampoline.
    pub stack: StackHandle,
    /// Initial trusted instruction entry point.
    pub entry: KernelEntry,
    /// Optional TLS allocation.
    pub tls: TlsHandle,
}

/// Versioned generation-bearing thread identity for runtime context binding.
///
/// The explicit fields keep the scheduler's private integer encoding out of OS
/// runtime implementations while remaining a value-only trait-FFI type.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct ThreadIdentityV1 {
    /// Task-system registry slot.
    pub slot: u32,
    /// Non-zero reuse generation for `slot`.
    pub generation: u32,
}

impl ThreadIdentityV1 {
    /// Creates a runtime identity from its explicit generation-bearing parts.
    pub const fn new(slot: u32, generation: u32) -> Self {
        Self { slot, generation }
    }
}

/// Immutable association between one runtime context and scheduler identity.
///
/// Contexts are created before the scheduler allocates a generation-bearing
/// thread ID. The scheduler submits this value exactly once after ID allocation
/// and before the thread can become `Ready`. Keeping both fields scalar makes
/// the operation suitable for the trait-FFI boundary without exporting a
/// scheduler object, reference, or ownership-bearing handle.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct ContextThreadBinding {
    /// Live runtime-owned execution context to bind.
    pub context: ExecutionContextHandle,
    /// Typed scheduler identity without exposing [`crate::ThreadId`]'s encoding.
    pub identity: ThreadIdentityV1,
}

/// Allocation requirements for a thread-local storage area.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct TlsRequest {
    /// TLS template start, or zero when no template is required.
    pub template_start: usize,
    /// Bytes copied from the template.
    pub initialized_size: usize,
    /// Total allocation size including zero-filled bytes.
    pub total_size: usize,
    /// Required allocation alignment.
    pub alignment: usize,
}

/// Allocation-free scheduler switch diagnostic record.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct SchedSwitchRecord {
    /// Logical CPU performing the switch.
    pub cpu: RuntimeCpuId,
    /// Previous generation-based thread identifier encoded as a scalar.
    pub previous_thread: u64,
    /// Next generation-based thread identifier encoded as a scalar.
    pub next_thread: u64,
    /// Monotonic switch timestamp.
    pub timestamp_ns: u64,
    /// Policy-specific reason code defined by ax-task.
    pub reason: u32,
}

/// Absolute finite, non-zero deadline measured by the runtime's monotonic clock.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct MonotonicDeadline(u64);

impl MonotonicDeadline {
    /// Creates a representable physical clockevent deadline.
    ///
    /// Zero is the uninitialized sentinel and `u64::MAX` represents no finite
    /// deadline; neither may cross the runtime hardware boundary.
    pub const fn from_nanos(deadline_ns: u64) -> Option<Self> {
        if deadline_ns == 0 || deadline_ns == u64::MAX {
            None
        } else {
            Some(Self(deadline_ns))
        }
    }

    /// Returns the absolute deadline in nanoseconds.
    pub const fn as_nanos(self) -> u64 {
        self.0
    }
}

/// One generation-ordered publication from a CPU's task-deadline owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TaskDeadlineUpdate {
    generation: u64,
    deadline: Option<MonotonicDeadline>,
    deferred_work: bool,
}

impl TaskDeadlineUpdate {
    /// Creates one publication after the owner has committed its local state.
    ///
    /// Generation zero is reserved for an uninitialized consumer.
    pub const fn try_new(
        generation: u64,
        deadline: Option<MonotonicDeadline>,
        deferred_work: bool,
    ) -> Option<Self> {
        if generation == 0 {
            None
        } else {
            Some(Self {
                generation,
                deadline,
                deferred_work,
            })
        }
    }

    /// Returns the monotonically increasing per-CPU publication generation.
    pub const fn generation(self) -> u64 {
        self.generation
    }

    /// Returns the next task-owned deadline, if one exists.
    pub const fn deadline(self) -> Option<MonotonicDeadline> {
        self.deadline
    }

    /// Reports work that must be completed at a scheduler safe point.
    pub const fn deferred_work(self) -> bool {
        self.deferred_work
    }
}

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

    /// Returns the pinned CPU-local object for the calling CPU.
    ///
    /// This is a CPU-owned capability, not a migration-stable task handle. The
    /// caller must retain an IRQ guard or scheduler-frame baton from before this
    /// query until every dereference of the returned object has completed.
    ///
    /// # Safety
    ///
    /// A non-`NONE` result must identify the pinned [`crate::CpuLocal`] owned by
    /// the calling CPU and kept live until shutdown. Its address must originate
    /// from the allocation's mutable owner capability, not from a shared
    /// `CpuLocal` borrow. Before reconstructing a reference, the caller must
    /// claim the matching [`crate::CpuRemote`] gate and retain both that claim
    /// and its CPU pin for the complete derived-borrow lifetime. Runtime-side
    /// direct owner accesses must use the same gate as the ax-task facade.
    unsafe fn current_cpu_local_handle() -> CurrentCpuLocalHandle;

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

    /// Returns the number of CPUs published online to the scheduler.
    fn online_cpu_count() -> u32;

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
    /// invoke callbacks, consume the scheduler baton, or re-enter ax-task. A
    /// Any failure is an unrecoverable runtime invariant: the raw switch has
    /// already committed, so there is no compatibility retry path.
    fn finish_context_switch_tail();

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

    /// Returns monotonic time in nanoseconds.
    fn monotonic_ns() -> u64;

    /// Returns the smallest programmable timer interval in nanoseconds.
    fn timer_resolution_ns() -> u64;

    /// Commits the current CPU's complete task-deadline state.
    ///
    /// The runtime owns the physical clockevent. It must ignore generations
    /// older than the most recently accepted update and merge the accepted
    /// task deadline with non-task sources before programming hardware. This
    /// hook is callable from ordinary task context, so the runtime must hold
    /// local IRQ exclusion across both state publication and hardware
    /// programming instead of relying on an implicit caller-side guard.
    ///
    /// This is an infallible ownership boundary, like Linux's hrtimer-to-
    /// clockevent rearm path. A runtime must absorb an expired hardware
    /// deadline with its minimum-delta fallback and treat a device that cannot
    /// retain a wakeup source as a runtime-fatal invariant. Returning a
    /// recoverable error here would leave the scheduler queue and physical
    /// clockevent in an unknowable half-committed state.
    fn publish_task_deadline(update: TaskDeadlineUpdate);

    /// Sends a coalescible scheduler IPI directly to `cpu`.
    ///
    /// [`RuntimeStatus::Success`] means this call published a new physical
    /// delivery. [`RuntimeStatus::Busy`] is a specialized success result: an
    /// older delivery for `cpu` is still in flight and is guaranteed to cover
    /// all scheduler work published before this call. The runtime must not
    /// return `Busy` merely because the transport could not accept a request;
    /// doing so would leave an idle target with no interrupt edge. Every other
    /// status is an unrecoverable violation of the scheduler delivery contract.
    fn send_scheduler_ipi(cpu: RuntimeCpuId) -> RuntimeStatus;

    /// Commits one local interrupt wait after the scheduler clears polling.
    ///
    /// The implementation must disable local interrupts, recheck sticky task
    /// work and physical clockevent state, and use the architecture's atomic
    /// IRQ-enable-and-wait primitive only when both remain idle. Work published
    /// before polling was cleared is observed by this final recheck; work
    /// published afterwards owns a physical interrupt edge.
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

    /// Switches from `previous` to `next` with local interrupts disabled.
    ///
    /// # Safety
    ///
    /// Both handles must identify live contexts owned by the runtime. The
    /// caller must have committed scheduler state and released runqueue locks.
    unsafe fn switch_context(previous: ExecutionContextHandle, next: ExecutionContextHandle);

    /// Activates the next context's explicit address-space state.
    ///
    /// [`AddressSpaceActivationKind::KernelLazy`] follows the architecture's
    /// current Linux lazy-TLB rule. The runtime keeps any borrowed active-mm
    /// owner unreclaimable until a later user activation or CPU-offline
    /// transaction releases that CPU lease.
    fn activate_address_space(activation: AddressSpaceActivation) -> RuntimeStatus;

    /// Flushes the current address space's local translation cache.
    fn flush_tlb_local(start: usize, size: usize);

    /// Emits an allocation-free context-switch trace record.
    fn trace_sched_switch(record: SchedSwitchRecord);

    /// Reports an unrecoverable scheduler invariant and terminates execution.
    fn fatal_invariant(code: u32, argument: usize) -> !;
}
