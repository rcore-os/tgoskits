//! Move-only runtime resources and architecture context ABI.

use alloc::sync::Arc;
use core::{marker::PhantomData, ptr::NonNull};

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
    /// Opaque pointer to the Arc-backed scheduler core of the current thread.
    ///
    /// This value is useful only as part of a runtime-provided
    /// [`CurrentThreadPublication`]. The scheduler may acquire a strong handle
    /// from it only while a preemption pin proves that the published thread is
    /// still current and therefore retains its owner-side strong reference.
    CurrentThreadOwnerHandle
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

/// Move-only runtime transaction for one committed scheduler switch.
///
/// ax-task constructs this value only after the scheduler has committed two
/// distinct live endpoints and released its internal locks. The execution
/// contexts and logical address spaces travel through one runtime call, so a
/// provider cannot activate an `mm` and then fail before preparing the matching
/// architecture context. Consuming the transaction prevents replay.
#[derive(Debug, Eq, PartialEq)]
#[repr(C)]
pub struct RuntimeSwitchPlan {
    previous_context: ExecutionContextHandle,
    previous_address_space: AddressSpaceHandle,
    next_context: ExecutionContextHandle,
    next_address_space: AddressSpaceHandle,
}

impl RuntimeSwitchPlan {
    pub(crate) fn new(
        previous_context: ExecutionContextHandle,
        previous_address_space: AddressSpaceHandle,
        next_context: ExecutionContextHandle,
        next_address_space: AddressSpaceHandle,
    ) -> Option<Self> {
        if previous_context.is_none() || next_context.is_none() || previous_context == next_context
        {
            None
        } else {
            Some(Self {
                previous_context,
                previous_address_space,
                next_context,
                next_address_space,
            })
        }
    }

    /// Returns the outgoing runtime context.
    pub const fn previous_context(&self) -> ExecutionContextHandle {
        self.previous_context
    }

    /// Returns the outgoing scheduler-selected logical address space.
    pub const fn previous_address_space(&self) -> AddressSpaceHandle {
        self.previous_address_space
    }

    /// Returns the incoming runtime context.
    pub const fn next_context(&self) -> ExecutionContextHandle {
        self.next_context
    }

    /// Returns the incoming scheduler-selected logical address space.
    pub const fn next_address_space(&self) -> AddressSpaceHandle {
        self.next_address_space
    }
}
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

/// Stable identity of one Linux-style address-space generation.
///
/// Distinct scheduler resource tokens may carry different
/// [`AddressSpaceHandle`] values while referring to the same shared `mm`.
/// Runtime providers must therefore derive this identity from the shared
/// address-space owner rather than from the token allocation itself.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(transparent)]
pub struct AddressSpaceMembarrierId(usize);

impl AddressSpaceMembarrierId {
    /// Identity used by kernel threads which do not own a userspace `mm`.
    pub const NONE: Self = Self(0);

    /// Creates an identity from a runtime-owned shared address-space object.
    ///
    /// # Safety
    ///
    /// A non-zero value must remain unique for the complete lifetime of the
    /// corresponding address-space generation. It must not be reused while an
    /// [`AddressSpaceMembarrierState`] containing it can remain rq-visible.
    pub const unsafe fn from_raw(raw: usize) -> Self {
        Self(raw)
    }

    /// Returns whether this is the kernel-thread sentinel.
    pub const fn is_none(self) -> bool {
        self.0 == 0
    }

    /// Returns the provider-owned opaque identity.
    pub const fn into_raw(self) -> usize {
        self.0
    }
}

/// One membarrier facility registered by an address space.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum MembarrierRegistration {
    /// Enables process-independent expedited barriers for this `mm`.
    GlobalExpedited  = 1,
    /// Enables expedited barriers restricted to this `mm`.
    PrivateExpedited = 2,
}

impl MembarrierRegistration {
    /// Returns the bit stored while rq synchronization is in progress.
    pub const fn requested_bit(self) -> u32 {
        self as u32
    }

    /// Returns the bit published only after every running rq is synchronized.
    pub const fn ready_bit(self) -> u32 {
        (self as u32) << 16
    }
}

/// Phase of one irreversible per-address-space registration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum MembarrierRegistrationPhase {
    /// Publishes the requested bit before inspecting any runqueue.
    Begin    = 0,
    /// Publishes the ready bit after synchronous rq refresh completes.
    Complete = 1,
}

/// Allocation-free snapshot of one address space's membarrier state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct AddressSpaceMembarrierState {
    identity: AddressSpaceMembarrierId,
    bits: u32,
}

impl AddressSpaceMembarrierState {
    /// State installed for a kernel thread without a userspace `mm`.
    pub const NONE: Self = Self {
        identity: AddressSpaceMembarrierId::NONE,
        bits: 0,
    };

    /// Constructs a provider snapshot from one live shared `mm` identity.
    ///
    /// # Safety
    ///
    /// `identity` must obey [`AddressSpaceMembarrierId::from_raw`], and `bits`
    /// must contain only requested and ready bits produced by
    /// [`MembarrierRegistration`].
    pub const unsafe fn new(identity: AddressSpaceMembarrierId, bits: u32) -> Self {
        Self { identity, bits }
    }

    /// Returns the shared address-space identity.
    pub const fn identity(self) -> AddressSpaceMembarrierId {
        self.identity
    }

    /// Reports whether registration has begun, including its synchronization
    /// interval before the ready bit becomes visible.
    pub const fn requested(self, registration: MembarrierRegistration) -> bool {
        self.bits & registration.requested_bit() != 0
    }

    /// Reports whether registration completed its rq synchronization.
    pub const fn ready(self, registration: MembarrierRegistration) -> bool {
        self.bits & registration.ready_bit() != 0
    }

    /// Reports whether any scheduler-visible membarrier facility is active.
    pub const fn any_requested(self) -> bool {
        self.bits
            & (MembarrierRegistration::GlobalExpedited.requested_bit()
                | MembarrierRegistration::PrivateExpedited.requested_bit())
            != 0
    }

    /// Returns the provider-owned atomic representation.
    pub const fn bits(self) -> u32 {
        self.bits
    }
}

pub(crate) const fn scheduled_membarrier_state(
    active_mm_state: AddressSpaceMembarrierState,
    task_membarrier_state: AddressSpaceMembarrierState,
) -> AddressSpaceMembarrierState {
    if task_membarrier_state.identity().is_none() {
        active_mm_state
    } else {
        task_membarrier_state
    }
}

#[cfg(axtest)]
pub const fn scheduled_membarrier_state_for_test(
    active_mm_state: AddressSpaceMembarrierState,
    task_membarrier_state: AddressSpaceMembarrierState,
) -> AddressSpaceMembarrierState {
    scheduled_membarrier_state(active_mm_state, task_membarrier_state)
}

/// Bounded operation executed synchronously on a target CPU.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum RuntimeMembarrierAction {
    /// Executes a full memory barrier in hard-IRQ context.
    MemoryBarrier   = 0,
    /// Refreshes `rq->membarrier_state` from its current dispatch and executes
    /// the corresponding full barrier.
    RefreshRunQueue = 1,
}

/// Immutable scheduler snapshot of one thread's runtime switch bindings.
///
/// Linux keeps the architecture context and `mm` selected by the rq transition
/// reachable without taking a second task lock after `pick_next_task()`.  The
/// ax-task owner rq follows the same rule: task-control code republishes this
/// value whenever the binding changes, and the switch plan consumes only the
/// rq-owned snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ThreadRuntimeBinding {
    context: ExecutionContextHandle,
    address_space: AddressSpaceHandle,
}

impl ThreadRuntimeBinding {
    pub(crate) const fn new(
        context: ExecutionContextHandle,
        address_space: AddressSpaceHandle,
    ) -> Self {
        Self {
            context,
            address_space,
        }
    }

    pub(crate) const fn context(self) -> ExecutionContextHandle {
        self.context
    }

    pub(crate) const fn address_space(self) -> AddressSpaceHandle {
        self.address_space
    }
}

/// Unique destruction right for one runtime-owned address-space object.
///
/// The scheduler may copy [`AddressSpaceHandle`] values derived from this
/// token into dispatch metadata, but exactly one token owns the eventual
/// [`crate::runtime::TaskRuntime::destroy_address_space`] operation.
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

/// Runtime-defined raw local-IRQ state saved by a synchronization guard.
///
/// Unlike [`IrqGuardToken`], this value does not own a scheduler publication
/// scope. It only transports the architecture interrupt state back to the
/// runtime that produced it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(transparent)]
pub struct LocalIrqState(usize);

impl LocalIrqState {
    /// Creates a saved local-IRQ state at the runtime provider boundary.
    ///
    /// # Safety
    ///
    /// `raw` must be a state value accepted by the linked runtime's matching
    /// local-IRQ restore operation.
    pub const unsafe fn from_raw(raw: usize) -> Self {
        Self(raw)
    }

    /// Returns the runtime-owned representation of this saved state.
    pub const fn into_raw(self) -> usize {
        self.0
    }
}

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

/// Runtime-owned capability snapshot for one pinned scheduler CPU.
///
/// The three fields are captured in one runtime operation, mirroring Linux's
/// direct `this_rq()` lookup. Keeping the logical identity together with the
/// owner-only and remote endpoints prevents the scheduler from resolving its
/// current CPU back through the global registry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct CurrentCpuOwnerHandles {
    cpu: RuntimeCpuId,
    local: CurrentCpuLocalHandle,
    remote: CpuRemoteHandle,
}

impl CurrentCpuOwnerHandles {
    /// Empty capability used when a scheduler-frame entry is rejected.
    pub const NONE: Self = Self {
        cpu: RuntimeCpuId::new(u32::MAX),
        local: CurrentCpuLocalHandle::NONE,
        remote: CpuRemoteHandle::NONE,
    };

    /// Creates one pinned current-CPU capability snapshot.
    ///
    /// # Safety
    ///
    /// `local` and `remote` must identify the owner-only and Arc-backed
    /// scheduler endpoints for `cpu`. Every non-empty handle must remain live
    /// until shutdown, and the caller must keep migration excluded while the
    /// snapshot is used.
    pub const unsafe fn new(
        cpu: RuntimeCpuId,
        local: CurrentCpuLocalHandle,
        remote: CpuRemoteHandle,
    ) -> Self {
        Self { cpu, local, remote }
    }

    /// Returns the logical CPU identity bound to both handles.
    pub const fn cpu(self) -> RuntimeCpuId {
        self.cpu
    }

    /// Returns the current CPU's owner-only scheduler handle.
    pub const fn local(self) -> CurrentCpuLocalHandle {
        self.local
    }

    /// Returns the current CPU's Arc-backed remote endpoint.
    pub const fn remote(self) -> CpuRemoteHandle {
        self.remote
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
    /// A repeated IRQ-return pass after the previous scheduler frame fully
    /// released its switch baton.
    ///
    /// The caller enters with hardware IRQs disabled and preemption depth zero.
    /// Before claiming the fresh scheduler baton, the runtime establishes one
    /// ordinary preemption depth, opens the Linux-style IRQ window, disables
    /// IRQs again, and atomically converts that depth into the scheduler baton.
    IrqReturnContinuation = 4,
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

/// Immutable scheduler publication owned by one runtime execution context.
///
/// This is the Rust equivalent of Linux's architecture-selected `current`
/// pointer: the identity and its Arc-backed owner address are installed once
/// before the context can run, then remain immutable across preemption and
/// migration. The owner address is never a standalone weak or strong handle.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct CurrentThreadPublication {
    identity: ThreadIdentityV1,
    owner: CurrentThreadOwnerHandle,
}

/// Atomic runtime result of claiming one scheduler frame.
///
/// A successful result carries every immutable capability selected under the
/// same IRQ-off CPU pin: task system, owner CPU endpoints, and current thread.
/// This is the runtime boundary equivalent of Linux deriving `rq` and
/// `rq->curr` from one pinned scheduler entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct RuntimeSchedulerFrameEnterResult {
    status: RuntimeStatus,
    system: TaskSystemHandle,
    cpu: CurrentCpuOwnerHandles,
    current: CurrentThreadPublication,
}

impl RuntimeSchedulerFrameEnterResult {
    /// Creates a successful scheduler-frame capability snapshot.
    ///
    /// # Safety
    ///
    /// All handles and the current publication must describe the execution
    /// context pinned by the scheduler baton that was claimed in the same
    /// runtime transaction.
    pub const unsafe fn success(
        system: TaskSystemHandle,
        cpu: CurrentCpuOwnerHandles,
        current: CurrentThreadPublication,
    ) -> Self {
        Self {
            status: RuntimeStatus::Success,
            system,
            cpu,
            current,
        }
    }

    /// Creates a rejected scheduler-frame result without live capabilities.
    pub const fn failure(status: RuntimeStatus) -> Self {
        Self {
            status,
            system: TaskSystemHandle::NONE,
            cpu: CurrentCpuOwnerHandles::NONE,
            current: CurrentThreadPublication::NONE,
        }
    }

    /// Returns the runtime entry status.
    pub const fn status(self) -> RuntimeStatus {
        self.status
    }

    /// Returns the pinned task-system capability.
    pub const fn system(self) -> TaskSystemHandle {
        self.system
    }

    /// Returns the pinned owner-CPU capability.
    pub const fn cpu(self) -> CurrentCpuOwnerHandles {
        self.cpu
    }

    /// Returns the architecture-selected current-thread publication.
    pub const fn current(self) -> CurrentThreadPublication {
        self.current
    }
}

/// Borrowed view of the scheduler-owned current-thread reference.
///
/// Unlike [`crate::ThreadHandle`], this capability does not acquire an
/// external lifetime lease. It is confined to the current execution context;
/// the architecture publication and scheduler-owned `rq->curr` reference keep
/// the pointed-to core alive until the synchronous operation returns.
pub(crate) struct CurrentThreadRef {
    identity: crate::ThreadId,
    core: NonNull<crate::ThreadCore>,
    _not_send: PhantomData<*mut ()>,
}

impl CurrentThreadRef {
    pub(crate) const fn id(&self) -> crate::ThreadId {
        self.identity
    }

    pub(crate) fn runtime_core(&self) -> &crate::ThreadCore {
        // SAFETY: construction validates the current publication while the
        // scheduler retains its owner-side reference. The borrow cannot
        // outlive this non-Send capability.
        unsafe { self.core.as_ref() }
    }
}

impl CurrentThreadPublication {
    /// Sentinel returned by an unbound bootstrap execution context.
    pub const NONE: Self = Self {
        identity: ThreadIdentityV1::NONE,
        owner: CurrentThreadOwnerHandle::NONE,
    };

    /// Returns the generation-bearing scheduler identity.
    pub const fn identity(self) -> ThreadIdentityV1 {
        self.identity
    }

    /// Returns the opaque current-owner address.
    pub const fn owner(self) -> CurrentThreadOwnerHandle {
        self.owner
    }

    pub(crate) fn from_core(identity: crate::ThreadId, core: &Arc<crate::ThreadCore>) -> Self {
        let owner = Arc::as_ptr(core).expose_provenance();
        // SAFETY: `core` supplies the live Arc allocation. Consumers may use
        // this address only through the checked current-publication accessors
        // while the matching runtime context remains the executing task.
        let owner = unsafe { CurrentThreadOwnerHandle::from_raw(owner) };
        Self {
            identity: ThreadIdentityV1::new(identity.slot(), identity.generation()),
            owner,
        }
    }

    /// Borrows the scheduler-owned current reference without creating an
    /// external handle or changing any Arc count.
    ///
    /// # Safety
    ///
    /// The runtime must have copied this publication from the architecture-
    /// selected current context. The caller must use the returned capability
    /// only in the synchronous operation of that context and must not exit the
    /// thread while it remains live.
    pub(crate) unsafe fn borrow_current(self) -> Result<CurrentThreadRef, crate::TaskError> {
        if !self.identity.is_bound() {
            return Err(crate::TaskError::NoRunnableThread);
        }
        let core = NonNull::new(core::ptr::with_exposed_provenance_mut::<crate::ThreadCore>(
            self.owner.into_raw(),
        ))
        .ok_or(crate::TaskError::InvalidRuntimeHandle)?;
        let identity = crate::ThreadId::from_parts(self.identity.slot, self.identity.generation);
        let current = CurrentThreadRef {
            identity,
            core,
            _not_send: PhantomData,
        };
        if current.runtime_core().id() != identity {
            return Err(crate::TaskError::InvalidRuntimeHandle);
        }
        Ok(current)
    }

    /// Acquires an ordinary external scheduler handle from the current
    /// context's owner publication.
    ///
    /// # Safety
    ///
    /// The runtime must have copied the publication from the architecture-
    /// selected current task context. The scheduler must retain that thread's
    /// owner-side `Arc` while the caller can execute or resume this operation.
    pub(crate) unsafe fn acquire_handle(self) -> Result<crate::ThreadHandle, crate::TaskError> {
        let core = unsafe {
            // SAFETY: this method has the same current-context ownership
            // contract as `acquire_scheduler_core`.
            self.acquire_scheduler_core()?
        };
        Ok(crate::ThreadHandle::from_core(core))
    }

    /// Acquires a scheduler-internal strong reference without publishing an
    /// external management lifetime lease.
    ///
    /// # Safety
    ///
    /// The runtime must have copied the publication from the architecture-
    /// selected current task context. The scheduler must retain that thread's
    /// owner-side `Arc` while the caller can execute or resume this operation.
    pub(crate) unsafe fn acquire_scheduler_core(
        self,
    ) -> Result<Arc<crate::ThreadCore>, crate::TaskError> {
        if !self.identity.is_bound() {
            return Err(crate::TaskError::NoRunnableThread);
        }
        if self.owner.is_none() {
            return Err(crate::TaskError::InvalidRuntimeHandle);
        }
        let core = core::ptr::with_exposed_provenance::<crate::ThreadCore>(self.owner.into_raw());
        // SAFETY: the current-task publication contract proves that an owner-
        // side strong reference remains live across preemption and migration.
        unsafe { Arc::increment_strong_count(core) };
        // SAFETY: the increment above created exactly one strong reference for
        // this reconstruction.
        let core = unsafe { Arc::from_raw(core) };
        let expected = crate::ThreadId::from_parts(self.identity.slot, self.identity.generation);
        if core.id() != expected {
            return Err(crate::TaskError::InvalidRuntimeHandle);
        }
        Ok(core)
    }
}

impl ThreadIdentityV1 {
    /// Sentinel returned before a runtime context is bound to a scheduler thread.
    pub const NONE: Self = Self {
        slot: 0,
        generation: 0,
    };

    /// Creates a runtime identity from its explicit generation-bearing parts.
    pub const fn new(slot: u32, generation: u32) -> Self {
        Self { slot, generation }
    }

    /// Returns whether this value names a published scheduler generation.
    pub const fn is_bound(self) -> bool {
        self.generation != 0
    }
}

/// Immutable association between one runtime context and scheduler ownership.
///
/// Contexts are created before the scheduler allocates a generation-bearing
/// thread ID. The scheduler submits this value exactly once after ID allocation
/// and before the thread can become runnable. The publication keeps only a
/// pointer-sized owner address; it does not transfer an Arc or external reaper
/// lease across the trait-FFI boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct ContextThreadBinding {
    /// Live runtime-owned execution context to bind.
    pub context: ExecutionContextHandle,
    /// Immutable current-thread publication for this execution context.
    pub publication: CurrentThreadPublication,
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

#[cfg(test)]
mod switch_plan_tests {
    use super::*;

    #[test]
    fn runtime_switch_plan_keeps_context_and_logical_mm_in_one_transaction() {
        // SAFETY: opaque values are never dereferenced by this value-only
        // contract test.
        let previous_context = unsafe { ExecutionContextHandle::from_raw(0x1000) };
        // SAFETY: see above.
        let next_context = unsafe { ExecutionContextHandle::from_raw(0x2000) };
        // SAFETY: see above.
        let previous_mm = unsafe { AddressSpaceHandle::from_raw(0x3000) };
        // SAFETY: see above.
        let next_mm = unsafe { AddressSpaceHandle::from_raw(0x4000) };

        let plan = RuntimeSwitchPlan::new(previous_context, previous_mm, next_context, next_mm)
            .expect("two distinct live contexts must form one runtime switch plan");

        assert_eq!(plan.previous_context(), previous_context);
        assert_eq!(plan.previous_address_space(), previous_mm);
        assert_eq!(plan.next_context(), next_context);
        assert_eq!(plan.next_address_space(), next_mm);
    }
}
