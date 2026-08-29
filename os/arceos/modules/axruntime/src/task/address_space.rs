//! Runtime-owned address-space tokens and per-CPU active-mm state.

use alloc::{boxed::Box, sync::Arc};
#[cfg(feature = "uspace")]
use core::sync::atomic::AtomicBool;
#[cfg(feature = "qperf-metrics")]
use core::sync::atomic::AtomicU64;
use core::{
    mem::align_of,
    ptr,
    sync::atomic::{AtomicU32, AtomicUsize, Ordering},
};

use ax_memory_addr::PhysAddr;
#[cfg(any(feature = "uspace", test))]
use ax_task::runtime::AddressSpaceActivationPhase;
use ax_task::{
    TaskError,
    runtime::{
        AddressSpaceActivation, AddressSpaceActivationKind, AddressSpaceDestroyOutcome,
        AddressSpaceHandle, AddressSpaceMembarrierId, AddressSpaceMembarrierState,
        AddressSpaceReclaimArmOutcome, AddressSpaceToken, MembarrierRegistration,
        MembarrierRegistrationPhase, RuntimeStatus,
    },
};

#[cfg(feature = "uspace")]
use super::with_current_cpu_pin;

/// OS-owned lifetime anchor retained by a scheduler address-space token.
trait TaskAddressSpaceOwner: Send + Sync {
    /// Releases ownership that follows the attached task while retaining any
    /// storage needed by CPUs that still carry the address space as lazy mm.
    fn detach_from_task(&self);
}

struct RetainedTaskAddressSpaceOwner<T>(T);

impl<T: Send + Sync> TaskAddressSpaceOwner for RetainedTaskAddressSpaceOwner<T> {
    fn detach_from_task(&self) {}
}

struct DetachableTaskAddressSpaceOwner<T> {
    owner: T,
    detached: core::sync::atomic::AtomicBool,
    detach: fn(&T),
}

impl<T: Send + Sync> TaskAddressSpaceOwner for DetachableTaskAddressSpaceOwner<T> {
    fn detach_from_task(&self) {
        if !self.detached.swap(true, Ordering::AcqRel) {
            (self.detach)(&self.owner);
        }
    }
}

struct RuntimeAddressSpace {
    #[cfg(feature = "uspace")]
    root: usize,
    active_cpus: AtomicUsize,
    reclaim_waiting: AtomicUsize,
    cpu_state: Arc<AddressSpaceCpuState>,
    _owner: Box<dyn TaskAddressSpaceOwner>,
}

const _: () = assert!(crate::CPU_CAPACITY <= usize::BITS as usize);

#[cfg(feature = "qperf-metrics")]
static ACTIVE_MM_SAME_ACTIVATIONS: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "qperf-metrics")]
static ACTIVE_MM_DIFFERENT_ACTIVATIONS: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "qperf-metrics")]
static ACTIVE_MM_KERNEL_LAZY_ACTIVATIONS: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "qperf-metrics")]
static ACTIVE_MM_HARDWARE_ROOT_WRITES: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "qperf-metrics")]
static ACTIVE_MM_LEASE_ACTIVATIONS: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "qperf-metrics")]
static ACTIVE_MM_LEASE_DEACTIVATIONS: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "qperf-metrics")]
static ACTIVE_MM_RECLAIM_READY: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "qperf-metrics")]
static ACTIVE_MM_RECLAIM_DESTROYED: AtomicU64 = AtomicU64::new(0);

#[cfg(feature = "qperf-metrics")]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct QperfAddressSpaceMetricsSnapshot {
    pub(super) same_activations: u64,
    pub(super) different_activations: u64,
    pub(super) kernel_lazy_activations: u64,
    pub(super) hardware_root_writes: u64,
    pub(super) lease_activations: u64,
    pub(super) lease_deactivations: u64,
    pub(super) reclaim_ready: u64,
    pub(super) reclaim_destroyed: u64,
}

#[cfg(feature = "qperf-metrics")]
pub(super) fn qperf_address_space_metrics_snapshot() -> QperfAddressSpaceMetricsSnapshot {
    QperfAddressSpaceMetricsSnapshot {
        same_activations: ACTIVE_MM_SAME_ACTIVATIONS.load(Ordering::Relaxed),
        different_activations: ACTIVE_MM_DIFFERENT_ACTIVATIONS.load(Ordering::Relaxed),
        kernel_lazy_activations: ACTIVE_MM_KERNEL_LAZY_ACTIVATIONS.load(Ordering::Relaxed),
        hardware_root_writes: ACTIVE_MM_HARDWARE_ROOT_WRITES.load(Ordering::Relaxed),
        lease_activations: ACTIVE_MM_LEASE_ACTIVATIONS.load(Ordering::Relaxed),
        lease_deactivations: ACTIVE_MM_LEASE_DEACTIVATIONS.load(Ordering::Relaxed),
        reclaim_ready: ACTIVE_MM_RECLAIM_READY.load(Ordering::Relaxed),
        reclaim_destroyed: ACTIVE_MM_RECLAIM_DESTROYED.load(Ordering::Relaxed),
    }
}

/// Shared CPU-footprint state for one hardware page-table root.
///
/// Every scheduler token for threads sharing one OS address space must carry
/// the same tracker. The runtime publishes a CPU bit before installing the
/// root and clears it only after replacing the hardware root, so page-table
/// mutation can target every CPU that may retain a translation.
pub struct AddressSpaceCpuState {
    root: usize,
    active_mask: AtomicUsize,
    membarrier_bits: AtomicU32,
}

impl AddressSpaceCpuState {
    /// Creates inactive runtime state permanently bound to one `mm` root.
    pub fn new(root: PhysAddr) -> Self {
        Self {
            root: root.as_usize(),
            active_mask: AtomicUsize::new(0),
            membarrier_bits: AtomicU32::new(0),
        }
    }

    fn matches_root(&self, root: PhysAddr) -> bool {
        self.root == root.as_usize()
    }

    /// Returns the CPUs that may currently retain translations for this root.
    pub fn active_mask(&self) -> usize {
        self.active_mask.load(Ordering::Acquire)
    }

    fn membarrier_state(this: &Arc<Self>) -> AddressSpaceMembarrierState {
        let raw = Arc::as_ptr(this).expose_provenance();
        // SAFETY: every scheduler token and `AddrSpace` owner retains this Arc,
        // so its allocation cannot be reused while the identity is rq-visible.
        let identity = unsafe { AddressSpaceMembarrierId::from_raw(raw) };
        let bits = this.membarrier_bits.load(Ordering::SeqCst);
        // SAFETY: `membarrier_bits` is changed only through the typed phase
        // update below and therefore contains only declared registration bits.
        unsafe { AddressSpaceMembarrierState::new(identity, bits) }
    }

    fn update_membarrier_state(
        this: &Arc<Self>,
        registration: MembarrierRegistration,
        phase: MembarrierRegistrationPhase,
    ) -> AddressSpaceMembarrierState {
        let bit = match phase {
            MembarrierRegistrationPhase::Begin => registration.requested_bit(),
            MembarrierRegistrationPhase::Complete => {
                assert!(
                    this.membarrier_bits.load(Ordering::SeqCst) & registration.requested_bit() != 0,
                    "membarrier registration completed before its requested phase"
                );
                registration.ready_bit()
            }
        };
        this.membarrier_bits.fetch_or(bit, Ordering::SeqCst);
        Self::membarrier_state(this)
    }

    #[cfg(any(feature = "uspace", test))]
    fn cpu_bit(cpu_id: usize) -> usize {
        1usize.checked_shl(cpu_id as u32).unwrap_or_else(|| {
            panic!("CPU {cpu_id} cannot be represented in an address-space mask")
        })
    }

    #[cfg(any(feature = "uspace", test))]
    fn activate(&self, cpu_id: usize) {
        self.active_mask
            .fetch_or(Self::cpu_bit(cpu_id), Ordering::Release);
    }

    #[cfg(any(feature = "uspace", test))]
    fn deactivate(&self, cpu_id: usize) {
        self.active_mask
            .fetch_and(!Self::cpu_bit(cpu_id), Ordering::Release);
    }
}

/// Move-only runtime token for one user address space.
pub struct TaskAddressSpace(Option<AddressSpaceToken>);

impl TaskAddressSpace {
    /// Creates a scheduler token that owns `owner` until address-space reap.
    pub fn new(root: PhysAddr, owner: impl Send + Sync + 'static) -> Result<Self, TaskError> {
        Self::new_with_owner(
            root,
            Arc::new(AddressSpaceCpuState::new(root)),
            Box::new(RetainedTaskAddressSpaceOwner(owner)),
        )
    }

    /// Creates a scheduler token with task-detach ownership semantics.
    ///
    /// `detach` runs once in ordinary task context after the attached thread
    /// has entered lazy kernel-mm state. It may release task-scoped accounting,
    /// but `owner` must keep the hardware page-table root valid until its final
    /// drop after all active-CPU leases have drained.
    pub fn new_with_task_detach<T: Send + Sync + 'static>(
        root: PhysAddr,
        cpu_state: Arc<AddressSpaceCpuState>,
        owner: T,
        detach: fn(&T),
    ) -> Result<Self, TaskError> {
        Self::new_with_owner(
            root,
            cpu_state,
            Box::new(DetachableTaskAddressSpaceOwner {
                owner,
                detached: core::sync::atomic::AtomicBool::new(false),
                detach,
            }),
        )
    }

    fn new_with_owner(
        root: PhysAddr,
        cpu_state: Arc<AddressSpaceCpuState>,
        owner: Box<dyn TaskAddressSpaceOwner>,
    ) -> Result<Self, TaskError> {
        if root.as_usize() == 0 || !cpu_state.matches_root(root) {
            return Err(TaskError::InvalidRuntimeHandle);
        }
        let address_space = Box::new(RuntimeAddressSpace {
            #[cfg(feature = "uspace")]
            root: root.as_usize(),
            active_cpus: AtomicUsize::new(0),
            reclaim_waiting: AtomicUsize::new(0),
            cpu_state,
            _owner: owner,
        });
        let raw = Box::into_raw(address_space).expose_provenance();
        // SAFETY: the fresh allocation transfers its unique destruction right
        // into this move-only token.
        Ok(Self(Some(unsafe { AddressSpaceToken::from_raw(raw) })))
    }

    pub(super) fn handle(&self) -> AddressSpaceHandle {
        self.0
            .as_ref()
            .unwrap_or_else(|| unreachable!("address-space token already transferred"))
            .handle()
    }

    #[cfg(feature = "uspace")]
    pub(super) fn token_mut(&mut self) -> &mut AddressSpaceToken {
        self.0
            .as_mut()
            .unwrap_or_else(|| unreachable!("address-space token already transferred"))
    }

    pub(super) fn take_token(&mut self) -> AddressSpaceToken {
        self.0
            .take()
            .unwrap_or_else(|| unreachable!("address-space token already transferred"))
    }
}

fn detach_runtime_address_space_owner(address_space: AddressSpaceHandle) {
    runtime_address_space(address_space)
        .unwrap_or_else(|_| panic!("address-space detach received an invalid owning handle"))
        ._owner
        .detach_from_task();
}

impl Drop for TaskAddressSpace {
    fn drop(&mut self) {
        let Some(address_space) = self.0.take() else {
            return;
        };
        detach_runtime_address_space_owner(address_space.handle());
        let outcome = destroy_runtime_address_space(address_space.handle());
        assert_eq!(
            outcome,
            AddressSpaceDestroyOutcome::Released,
            "unpublished address space retained an active CPU lease"
        );
    }
}

#[ax_percpu::def_percpu]
static ACTIVE_ADDRESS_SPACE: usize = 0;

/// Last-active-mm notification claimed before the raw context switch.
///
/// The incoming switch tail consumes this bit and returns it to ax-task. The
/// scheduler publishes task work only after releasing the outgoing task's
/// `on_cpu` claim, matching Linux `finish_task_switch()` ordering.
#[ax_percpu::def_percpu]
#[cfg(feature = "uspace")]
static CONTEXT_SWITCH_RECLAIM_READY: AtomicBool = AtomicBool::new(false);

fn runtime_address_space(
    address_space: AddressSpaceHandle,
) -> Result<&'static RuntimeAddressSpace, RuntimeStatus> {
    let raw = address_space.into_raw();
    if raw == 0 || !raw.is_multiple_of(align_of::<RuntimeAddressSpace>()) {
        return Err(RuntimeStatus::InvalidHandle);
    }
    let address_space = ptr::with_exposed_provenance::<RuntimeAddressSpace>(raw);
    // SAFETY: a borrowed handle is reachable only while its owning token or a
    // per-CPU active lease keeps this allocation live.
    Ok(unsafe { &*address_space })
}

#[cfg(feature = "uspace")]
pub(super) fn validate_address_space_handle(
    address_space: AddressSpaceHandle,
) -> Result<(), RuntimeStatus> {
    runtime_address_space(address_space).map(|_| ())
}

#[cfg(feature = "uspace")]
fn offline_kernel_root() -> usize {
    if cfg!(any(target_arch = "x86_64", target_arch = "riscv64")) {
        // SAFETY: CPU offline holds IRQ exclusion, and bring-up published the
        // immutable root before the CPU became scheduler-visible.
        unsafe { with_current_cpu_pin(super::bootstrap::offline_kernel_root) }
    } else {
        // AArch64 and LoongArch keep kernel mappings in their separate upper
        // root. Zero leaves no lower/user translation active while offline.
        0
    }
}

#[cfg(feature = "uspace")]
pub(super) fn current_hardware_root() -> usize {
    ax_hal::asm::read_user_page_table().as_usize()
}

#[cfg(any(feature = "uspace", test))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HardwareAddressSpaceTransition {
    SameAddressSpace,
    DifferentAddressSpace,
}

#[cfg(any(feature = "uspace", test))]
fn hardware_root_install_required(
    current_root: usize,
    next_root: usize,
    transition: HardwareAddressSpaceTransition,
) -> bool {
    current_root != next_root || transition == HardwareAddressSpaceTransition::DifferentAddressSpace
}

#[cfg(feature = "uspace")]
fn install_hardware_root(root: usize, transition: HardwareAddressSpaceTransition) {
    if hardware_root_install_required(current_hardware_root(), root, transition) {
        let root = ax_memory_addr::PhysAddr::from(root);
        // SAFETY: callers retain local IRQ exclusion for the complete active-mm
        // transaction.
        unsafe { ax_hal::asm::write_user_page_table(root) };
        #[cfg(feature = "qperf-metrics")]
        ACTIVE_MM_HARDWARE_ROOT_WRITES.fetch_add(1, Ordering::Relaxed);
        // Linux reloads CR3 when the logical mm changes even if a reclaimed
        // page-table frame gives the new mm the same root address. Otherwise
        // non-PCID x86 can retain translations from the former mm. The other
        // architecture backends only update their root register and require an
        // explicit invalidation for the same identity transition.
        #[cfg(not(target_arch = "x86_64"))]
        ax_hal::asm::flush_tlb(None);
    }
}

#[cfg(feature = "uspace")]
fn enter_lazy_kernel_address_space() {
    // Linux's current x86, RISC-V and LoongArch enter_lazy_tlb paths retain the
    // loaded user root and only change scheduler/ASID bookkeeping. AArch64
    // installs its reserved lower root so a kernel thread cannot use the
    // previous task's user mappings.
    #[cfg(target_arch = "aarch64")]
    install_hardware_root(0, HardwareAddressSpaceTransition::DifferentAddressSpace);
}

#[cfg(feature = "uspace")]
fn commit_user_address_space_activation(
    cpu_id: usize,
    previous_raw: usize,
    previous: Option<&RuntimeAddressSpace>,
    next_raw: usize,
    next: &RuntimeAddressSpace,
    install_root: impl FnOnce(usize, HardwareAddressSpaceTransition),
    publish_active: impl FnOnce(usize),
) -> bool {
    let same_address_space = next_raw == previous_raw
        || previous.is_some_and(|previous| Arc::ptr_eq(&previous.cpu_state, &next.cpu_state));
    if same_address_space {
        #[cfg(feature = "qperf-metrics")]
        ACTIVE_MM_SAME_ACTIVATIONS.fetch_add(1, Ordering::Relaxed);
        debug_assert_eq!(
            previous.map(|previous| previous.root),
            Some(next.root),
            "one address-space CPU tracker cannot describe different roots"
        );
        // Match Linux arm64's `enter_lazy_tlb()`/`switch_mm_irqs_off()` pair:
        // retain the active-mm lease, but restore the user root if a kernel
        // thread temporarily installed the reserved lower root. The runtime
        // backend suppresses the write when the hardware root is already
        // correct, so user-to-user switches in the same mm remain a no-op.
        install_root(next.root, HardwareAddressSpaceTransition::SameAddressSpace);
        return false;
    }
    #[cfg(feature = "qperf-metrics")]
    ACTIVE_MM_DIFFERENT_ACTIVATIONS.fetch_add(1, Ordering::Relaxed);
    next.active_cpus.fetch_add(1, Ordering::AcqRel);
    next.cpu_state.activate(cpu_id);
    #[cfg(feature = "qperf-metrics")]
    ACTIVE_MM_LEASE_ACTIVATIONS.fetch_add(1, Ordering::Relaxed);
    install_root(
        next.root,
        HardwareAddressSpaceTransition::DifferentAddressSpace,
    );
    publish_active(next_raw);
    if let Some(previous) = previous {
        previous.cpu_state.deactivate(cpu_id);
        #[cfg(feature = "qperf-metrics")]
        ACTIVE_MM_LEASE_DEACTIVATIONS.fetch_add(1, Ordering::Relaxed);
        release_active_cpu(previous)
    } else {
        false
    }
}

pub(super) fn activate_runtime_address_space(activation: AddressSpaceActivation) -> RuntimeStatus {
    #[cfg(feature = "uspace")]
    {
        // SAFETY: the scheduler baton or exec IRQ guard pins this operation to
        // one CPU until the active-mm publication is complete.
        unsafe {
            with_current_cpu_pin(|pin| {
                let previous_raw = ACTIVE_ADDRESS_SPACE.read_current(pin);
                if activation.kind() == AddressSpaceActivationKind::KernelLazy {
                    #[cfg(feature = "qperf-metrics")]
                    ACTIVE_MM_KERNEL_LAZY_ACTIVATIONS.fetch_add(1, Ordering::Relaxed);
                    enter_lazy_kernel_address_space();
                    return RuntimeStatus::Success;
                }

                let Some(address_space) = activation.user_handle() else {
                    return RuntimeStatus::InvalidHandle;
                };

                let next = match runtime_address_space(address_space) {
                    Ok(next) => next,
                    Err(status) => return status,
                };
                let next_raw = address_space.into_raw();
                let cpu_id = pin.area().cpu_index().as_usize();
                let previous = if previous_raw == 0 {
                    None
                } else {
                    let previous = AddressSpaceHandle::from_raw(previous_raw);
                    Some(
                        runtime_address_space(previous)
                            .unwrap_or_else(|_| panic!("active address-space handle became stale")),
                    )
                };
                let reclaim_ready = commit_user_address_space_activation(
                    cpu_id,
                    previous_raw,
                    previous,
                    next_raw,
                    next,
                    install_hardware_root,
                    |active| ACTIVE_ADDRESS_SPACE.write_current(pin, active),
                );
                if reclaim_ready {
                    route_reclaim_notification(
                        activation.phase(),
                        || {
                            CONTEXT_SWITCH_RECLAIM_READY.with_current(pin, |pending| {
                                assert!(
                                    !pending.swap(true, Ordering::AcqRel),
                                    "context switch retained an unconsumed active-mm reclaim edge"
                                );
                            });
                        },
                        ax_task::notify_address_space_reclaim,
                    );
                }
                RuntimeStatus::Success
            })
        }
    }
    #[cfg(not(feature = "uspace"))]
    {
        if activation.kind() == AddressSpaceActivationKind::KernelLazy {
            RuntimeStatus::Success
        } else {
            RuntimeStatus::Unsupported
        }
    }
}

pub(super) fn release_current_active_address_space() {
    #[cfg(feature = "uspace")]
    let reclaim_ready = unsafe {
        with_current_cpu_pin(|pin| {
            let previous_raw = ACTIVE_ADDRESS_SPACE.read_current(pin);
            if previous_raw == 0 {
                return false;
            }
            install_hardware_root(
                offline_kernel_root(),
                HardwareAddressSpaceTransition::DifferentAddressSpace,
            );
            ACTIVE_ADDRESS_SPACE.write_current(pin, 0);
            let previous = AddressSpaceHandle::from_raw(previous_raw);
            let previous = runtime_address_space(previous)
                .unwrap_or_else(|_| panic!("offline CPU retained a stale active address space"));
            previous
                .cpu_state
                .deactivate(pin.area().cpu_index().as_usize());
            #[cfg(feature = "qperf-metrics")]
            ACTIVE_MM_LEASE_DEACTIVATIONS.fetch_add(1, Ordering::Relaxed);
            release_active_cpu(previous)
        })
    };
    #[cfg(feature = "uspace")]
    if reclaim_ready {
        ax_task::notify_address_space_reclaim();
    }
}

pub(super) fn destroy_runtime_address_space(
    address_space: AddressSpaceHandle,
) -> AddressSpaceDestroyOutcome {
    let address_space = runtime_address_space(address_space)
        .unwrap_or_else(|_| panic!("address-space destruction received an invalid owning handle"));
    if address_space.active_cpus.load(Ordering::Acquire) != 0 {
        return AddressSpaceDestroyOutcome::Active;
    }
    let raw = address_space as *const RuntimeAddressSpace as *mut RuntimeAddressSpace;
    // SAFETY: the caller owns the unique AddressSpaceToken destruction right,
    // and the zero active count proves no CPU retains a borrowed pointer.
    drop(unsafe { Box::from_raw(raw) });
    #[cfg(feature = "qperf-metrics")]
    ACTIVE_MM_RECLAIM_DESTROYED.fetch_add(1, Ordering::Relaxed);
    AddressSpaceDestroyOutcome::Released
}

pub(super) fn arm_runtime_address_space_reclaim(
    address_space: AddressSpaceHandle,
) -> AddressSpaceReclaimArmOutcome {
    let address_space = runtime_address_space(address_space)
        .unwrap_or_else(|_| panic!("address-space reclaim arm received an invalid owning handle"));
    address_space.reclaim_waiting.store(1, Ordering::Release);
    if address_space.active_cpus.load(Ordering::Acquire) == 0 {
        address_space.reclaim_waiting.store(0, Ordering::Release);
        AddressSpaceReclaimArmOutcome::Ready
    } else {
        AddressSpaceReclaimArmOutcome::Armed
    }
}

pub(super) fn runtime_address_space_membarrier_state(
    address_space: AddressSpaceHandle,
) -> AddressSpaceMembarrierState {
    let address_space = runtime_address_space(address_space)
        .unwrap_or_else(|_| panic!("membarrier received an invalid address-space handle"));
    AddressSpaceCpuState::membarrier_state(&address_space.cpu_state)
}

pub(super) fn update_runtime_address_space_membarrier_state(
    address_space: AddressSpaceHandle,
    registration: MembarrierRegistration,
    phase: MembarrierRegistrationPhase,
) -> AddressSpaceMembarrierState {
    let address_space = runtime_address_space(address_space).unwrap_or_else(|_| {
        panic!("membarrier registration received an invalid address-space handle")
    });
    AddressSpaceCpuState::update_membarrier_state(&address_space.cpu_state, registration, phase)
}

#[cfg(any(feature = "uspace", test))]
fn release_active_cpu(address_space: &RuntimeAddressSpace) -> bool {
    let active = address_space.active_cpus.fetch_sub(1, Ordering::AcqRel);
    assert!(active >= 1, "active address-space CPU count underflow");
    let reclaim_ready = active == 1 && address_space.reclaim_waiting.swap(0, Ordering::AcqRel) != 0;
    #[cfg(feature = "qperf-metrics")]
    if reclaim_ready {
        ACTIVE_MM_RECLAIM_READY.fetch_add(1, Ordering::Relaxed);
    }
    reclaim_ready
}

#[cfg(any(feature = "uspace", test))]
fn route_reclaim_notification(
    phase: AddressSpaceActivationPhase,
    defer: impl FnOnce(),
    publish: impl FnOnce(),
) {
    match phase {
        AddressSpaceActivationPhase::CurrentTask => publish(),
        AddressSpaceActivationPhase::ContextSwitch => defer(),
    }
}

pub(super) fn take_context_switch_reclaim_ready() -> bool {
    #[cfg(feature = "uspace")]
    {
        // SAFETY: the incoming runtime switch tail retains the scheduler baton
        // and therefore cannot migrate while consuming the CPU-local edge.
        unsafe {
            with_current_cpu_pin(|pin| {
                CONTEXT_SWITCH_RECLAIM_READY
                    .with_current(pin, |pending| pending.swap(false, Ordering::AcqRel))
            })
        }
    }
    #[cfg(not(feature = "uspace"))]
    {
        false
    }
}

/// Replaces the running user task's owning address-space token.
pub fn switch_current_address_space(address_space: TaskAddressSpace) -> Result<(), TaskError> {
    #[cfg(feature = "uspace")]
    {
        let mut address_space = address_space;
        let previous = {
            let _irq = crate::sync::IrqSaveGuard::new();
            let next_handle = address_space.handle();
            validate_address_space_handle(next_handle).map_err(super::runtime_status_error)?;
            let previous = ax_task::replace_current_address_space(address_space.token_mut())?;

            // No fallible operation may follow the ownership transfer above.
            // The validated runtime object supplies the root, and the IRQ guard
            // keeps the CPU active-mm slot exclusive.
            let status = activate_runtime_address_space(AddressSpaceActivation::user(next_handle));
            assert_eq!(
                status,
                RuntimeStatus::Success,
                "validated address-space activation failed after ownership transfer"
            );
            let transferred = address_space.take_token();
            debug_assert!(transferred.is_none());
            previous
        };

        // Reclaim may allocate or drop an OS ownership anchor. It therefore
        // runs only after the exec transaction has restored normal IRQ state.
        ax_task::release_address_space_token(previous)
    }
    #[cfg(not(feature = "uspace"))]
    {
        let _ = address_space;
        Err(TaskError::RuntimeFailure(RuntimeStatus::Unsupported as u32))
    }
}

/// Detaches the running user task from its address space before exit
/// publication, matching Linux `exit_mm` ordering.
pub fn detach_current_address_space() -> Result<(), TaskError> {
    #[cfg(feature = "uspace")]
    {
        let previous = {
            let _irq = crate::sync::IrqSaveGuard::new();
            let previous = ax_task::detach_current_address_space()?;
            let status = activate_runtime_address_space(AddressSpaceActivation::KERNEL_LAZY);
            assert_eq!(
                status,
                RuntimeStatus::Success,
                "detaching current address space failed to enter lazy kernel-mm state"
            );
            previous
        };

        // The task-scoped owner may acquire sleepable OS locks. Run it only
        // after restoring IRQs, while the runtime wrapper still pins the root
        // for any CPU retaining it as a lazy active mm.
        detach_runtime_address_space_owner(previous.handle());
        ax_task::release_address_space_token(previous)
    }
    #[cfg(not(feature = "uspace"))]
    {
        Err(TaskError::RuntimeFailure(RuntimeStatus::Unsupported as u32))
    }
}

#[cfg(test)]
mod tests {
    use alloc::sync::Arc;
    use core::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    struct CountDrop(Arc<AtomicUsize>);

    impl Drop for CountDrop {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::Release);
        }
    }

    #[test]
    fn switch_activation_defers_reclaim_notification_until_switch_tail() {
        let deferred = AtomicUsize::new(0);
        let published = AtomicUsize::new(0);

        route_reclaim_notification(
            AddressSpaceActivationPhase::ContextSwitch,
            || {
                deferred.fetch_add(1, Ordering::Relaxed);
            },
            || {
                published.fetch_add(1, Ordering::Relaxed);
            },
        );

        assert_eq!(deferred.load(Ordering::Relaxed), 1);
        assert_eq!(published.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn active_cpu_lease_blocks_owner_destruction_until_release() {
        let drops = Arc::new(AtomicUsize::new(0));
        let mut token = TaskAddressSpace::new(
            ax_memory_addr::PhysAddr::from(0x4000),
            CountDrop(Arc::clone(&drops)),
        )
        .unwrap();
        let handle = token.handle();
        let runtime = runtime_address_space(handle).unwrap();
        runtime.active_cpus.fetch_add(1, Ordering::AcqRel);
        let owned = token.take_token();

        assert_eq!(
            destroy_runtime_address_space(handle),
            AddressSpaceDestroyOutcome::Active
        );
        assert_eq!(
            arm_runtime_address_space_reclaim(handle),
            AddressSpaceReclaimArmOutcome::Armed
        );
        assert_eq!(drops.load(Ordering::Acquire), 0);

        assert!(release_active_cpu(runtime));
        assert_eq!(
            destroy_runtime_address_space(handle),
            AddressSpaceDestroyOutcome::Released
        );
        assert_eq!(drops.load(Ordering::Acquire), 1);
        assert!(!owned.is_none());
    }

    #[test]
    fn task_detach_releases_task_owner_before_lazy_cpu_lease() {
        struct CountDetach {
            detaches: Arc<AtomicUsize>,
            drops: Arc<AtomicUsize>,
        }

        impl Drop for CountDetach {
            fn drop(&mut self) {
                self.drops.fetch_add(1, Ordering::Release);
            }
        }

        fn detach(owner: &CountDetach) {
            owner.detaches.fetch_add(1, Ordering::Release);
        }

        let detaches = Arc::new(AtomicUsize::new(0));
        let drops = Arc::new(AtomicUsize::new(0));
        let mut token = TaskAddressSpace::new_with_task_detach(
            ax_memory_addr::PhysAddr::from(0x4000),
            Arc::new(AddressSpaceCpuState::new(PhysAddr::from(0x4000))),
            CountDetach {
                detaches: Arc::clone(&detaches),
                drops: Arc::clone(&drops),
            },
            detach,
        )
        .unwrap();
        let handle = token.handle();
        let runtime = runtime_address_space(handle).unwrap();
        runtime.active_cpus.fetch_add(1, Ordering::AcqRel);
        let owned = token.take_token();

        detach_runtime_address_space_owner(handle);
        detach_runtime_address_space_owner(handle);
        assert_eq!(detaches.load(Ordering::Acquire), 1);
        assert_eq!(drops.load(Ordering::Acquire), 0);
        assert_eq!(
            destroy_runtime_address_space(handle),
            AddressSpaceDestroyOutcome::Active
        );

        assert!(!release_active_cpu(runtime));
        assert_eq!(
            destroy_runtime_address_space(handle),
            AddressSpaceDestroyOutcome::Released
        );
        assert_eq!(drops.load(Ordering::Acquire), 1);
        assert!(!owned.is_none());
    }

    #[test]
    fn shared_cpu_state_publishes_and_withdraws_cpu_footprints() {
        let tracker = AddressSpaceCpuState::new(PhysAddr::from(0x4000));

        tracker.activate(1);
        tracker.activate(3);
        assert_eq!(tracker.active_mask(), (1usize << 1) | (1usize << 3));

        tracker.deactivate(1);
        assert_eq!(tracker.active_mask(), 1usize << 3);
    }

    #[test]
    fn address_space_cpu_state_rejects_mismatched_root() {
        let tracker = Arc::new(AddressSpaceCpuState::new(PhysAddr::from(0x4000)));
        let token =
            TaskAddressSpace::new_with_task_detach(PhysAddr::from(0x8000), tracker, (), |_| {});

        assert!(matches!(token, Err(TaskError::InvalidRuntimeHandle)));
    }

    #[test]
    fn shared_mm_tokens_share_membarrier_identity_and_registration() {
        let cpu_state = Arc::new(AddressSpaceCpuState::new(PhysAddr::from(0x4000)));
        let first = TaskAddressSpace::new_with_task_detach(
            PhysAddr::from(0x4000),
            Arc::clone(&cpu_state),
            (),
            |_| {},
        )
        .unwrap();
        let second = TaskAddressSpace::new_with_task_detach(
            PhysAddr::from(0x4000),
            Arc::clone(&cpu_state),
            (),
            |_| {},
        )
        .unwrap();

        let requested = update_runtime_address_space_membarrier_state(
            first.handle(),
            MembarrierRegistration::PrivateExpedited,
            MembarrierRegistrationPhase::Begin,
        );
        let observed = runtime_address_space_membarrier_state(second.handle());
        assert_eq!(requested, observed);
        assert!(observed.requested(MembarrierRegistration::PrivateExpedited));
        assert!(!observed.ready(MembarrierRegistration::PrivateExpedited));

        let ready = update_runtime_address_space_membarrier_state(
            second.handle(),
            MembarrierRegistration::PrivateExpedited,
            MembarrierRegistrationPhase::Complete,
        );
        assert_eq!(
            runtime_address_space_membarrier_state(first.handle()),
            ready
        );
        assert!(ready.ready(MembarrierRegistration::PrivateExpedited));
    }

    #[cfg(feature = "uspace")]
    #[test]
    fn same_mm_activation_retains_the_existing_cpu_lease_without_hardware_work() {
        let tracker = Arc::new(AddressSpaceCpuState::new(PhysAddr::from(0x4000)));
        let previous = TaskAddressSpace::new_with_task_detach(
            PhysAddr::from(0x4000),
            Arc::clone(&tracker),
            (),
            |_| {},
        )
        .unwrap();
        let next = TaskAddressSpace::new_with_task_detach(
            PhysAddr::from(0x4000),
            Arc::clone(&tracker),
            (),
            |_| {},
        )
        .unwrap();
        let previous_runtime = runtime_address_space(previous.handle()).unwrap();
        let next_runtime = runtime_address_space(next.handle()).unwrap();
        previous_runtime.active_cpus.store(1, Ordering::Release);
        tracker.activate(0);
        let hardware_root = AtomicUsize::new(0x4000);
        let hardware_installs = AtomicUsize::new(0);
        let active_publications = AtomicUsize::new(0);

        let reclaim_ready = commit_user_address_space_activation(
            0,
            previous.handle().into_raw(),
            Some(previous_runtime),
            next.handle().into_raw(),
            next_runtime,
            |root, _transition| {
                if hardware_root.swap(root, Ordering::AcqRel) != root {
                    hardware_installs.fetch_add(1, Ordering::Relaxed);
                }
            },
            |_| {
                active_publications.fetch_add(1, Ordering::Relaxed);
            },
        );

        let previous_leases = previous_runtime.active_cpus.load(Ordering::Acquire);
        let next_leases = next_runtime.active_cpus.load(Ordering::Acquire);
        previous_runtime.active_cpus.store(0, Ordering::Release);
        next_runtime.active_cpus.store(0, Ordering::Release);
        tracker.deactivate(0);

        assert_eq!(previous_leases, 1);
        assert_eq!(next_leases, 0);
        assert_eq!(hardware_installs.load(Ordering::Relaxed), 0);
        assert_eq!(active_publications.load(Ordering::Relaxed), 0);
        assert!(!reclaim_ready);
    }

    #[cfg(feature = "uspace")]
    #[test]
    fn lazy_kernel_root_is_restored_before_same_mm_user_execution() {
        let tracker = Arc::new(AddressSpaceCpuState::new(PhysAddr::from(0x4000)));
        let previous = TaskAddressSpace::new_with_task_detach(
            PhysAddr::from(0x4000),
            Arc::clone(&tracker),
            (),
            |_| {},
        )
        .unwrap();
        let next = TaskAddressSpace::new_with_task_detach(
            PhysAddr::from(0x4000),
            Arc::clone(&tracker),
            (),
            |_| {},
        )
        .unwrap();
        let previous_runtime = runtime_address_space(previous.handle()).unwrap();
        let next_runtime = runtime_address_space(next.handle()).unwrap();
        previous_runtime.active_cpus.store(1, Ordering::Release);
        tracker.activate(0);
        let hardware_root = AtomicUsize::new(0);
        let hardware_installs = AtomicUsize::new(0);
        let active_publications = AtomicUsize::new(0);

        let reclaim_ready = commit_user_address_space_activation(
            0,
            previous.handle().into_raw(),
            Some(previous_runtime),
            next.handle().into_raw(),
            next_runtime,
            |root, _transition| {
                if hardware_root.swap(root, Ordering::AcqRel) != root {
                    hardware_installs.fetch_add(1, Ordering::Relaxed);
                }
            },
            |_| {
                active_publications.fetch_add(1, Ordering::Relaxed);
            },
        );

        let previous_leases = previous_runtime.active_cpus.load(Ordering::Acquire);
        let next_leases = next_runtime.active_cpus.load(Ordering::Acquire);
        previous_runtime.active_cpus.store(0, Ordering::Release);
        next_runtime.active_cpus.store(0, Ordering::Release);
        tracker.deactivate(0);

        assert_eq!(hardware_root.load(Ordering::Acquire), 0x4000);
        assert_eq!(hardware_installs.load(Ordering::Relaxed), 1);
        assert_eq!(previous_leases, 1);
        assert_eq!(next_leases, 0);
        assert_eq!(active_publications.load(Ordering::Relaxed), 0);
        assert!(!reclaim_ready);
    }

    #[test]
    fn different_address_space_reusing_same_root_requires_hardware_install() {
        assert!(hardware_root_install_required(
            0x4000,
            0x4000,
            HardwareAddressSpaceTransition::DifferentAddressSpace,
        ));
        assert!(!hardware_root_install_required(
            0x4000,
            0x4000,
            HardwareAddressSpaceTransition::SameAddressSpace,
        ));
    }
}
