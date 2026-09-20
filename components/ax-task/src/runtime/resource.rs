//! Runtime resource ownership and address-space lifecycle.

use crate::{
    runtime::context::{
        RuntimeIrqGuard, runtime_current_cpu_mut, runtime_task_system, validate_task_context,
    },
    thread::{TaskError, current::current_thread_id},
};
pub use crate::{
    runtime::service::reclaim::notify_address_space_reclaim, thread::spec::ThreadResources,
};

/// Returns the scheduler-selected logical address space of the current task.
///
/// This low-level runtime query is intended for the final user-entry
/// validation. The returned opaque handle does not transfer ownership.
#[doc(hidden)]
pub fn current_address_space_handle()
-> Result<crate::runtime::resource::AddressSpaceHandle, TaskError> {
    let current = current_thread_id()?;
    let mut irq = RuntimeIrqGuard::enter();
    let cpu = runtime_current_cpu_mut(&mut irq)?;
    // SAFETY: `irq` owns the IRQ-off owner-CPU scope and the architecture
    // current publication proved `current` belongs to this execution context.
    unsafe { cpu.scheduler_current_address_space(current) }
}

/// Replaces the current thread's scheduler-visible address-space token.
///
/// The runtime must update its architecture context and hardware page table in
/// the same outer IRQ-off transaction after this function returns. The old
/// token remains scheduler-owned and is returned so the runtime can defer its
/// task-context reclamation after leaving that IRQ-off transaction.
pub fn replace_current_address_space(
    address_space: &mut crate::runtime::resource::AddressSpaceToken,
) -> Result<crate::runtime::resource::AddressSpaceToken, TaskError> {
    validate_task_context()?;
    let mut irq = RuntimeIrqGuard::enter();
    let mut cpu = runtime_current_cpu_mut(&mut irq)?;
    runtime_task_system()?.replace_current_address_space(cpu.as_mut(), address_space)
}

/// Detaches the current thread's scheduler-visible user address space.
///
/// The runtime must enter its lazy kernel address-space state before the outer
/// IRQ-off transaction ends, then transfer the returned token to task-context
/// reclamation.
pub fn detach_current_address_space()
-> Result<crate::runtime::resource::AddressSpaceToken, TaskError> {
    validate_task_context()?;
    let mut irq = RuntimeIrqGuard::enter();
    let mut cpu = runtime_current_cpu_mut(&mut irq)?;
    runtime_task_system()?.detach_current_address_space(cpu.as_mut())
}

/// Transfers an obsolete address-space token to task-context reclamation.
///
/// The runtime may still report the object busy while another CPU retains it
/// as an active mm. The task-work reaper owns every retry after this function
/// accepts the token.
pub fn release_address_space_token(
    address_space: crate::runtime::resource::AddressSpaceToken,
) -> Result<(), TaskError> {
    validate_task_context()?;
    runtime_task_system()?.release_address_space_token(address_space);
    Ok(())
}
use crate::runtime::handle::opaque_handle;

opaque_handle!(
    /// Opaque handle to an architecture execution context.
    ExecutionContextHandle,
    "runtime::resource"
);
opaque_handle!(
    /// Opaque handle to a runtime-owned stack allocation.
    StackHandle,
    "runtime::resource"
);
opaque_handle!(
    /// Opaque handle to a runtime-owned TLS allocation.
    TlsHandle,
    "runtime::resource"
);
opaque_handle!(
    /// Borrowed opaque handle to a runtime-owned address space.
    AddressSpaceHandle,
    "runtime::resource"
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
