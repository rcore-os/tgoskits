// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! AxVM-owned architecture-independent vCPU wrapper.

use alloc::{format, sync::Arc};
use core::{
    cell::UnsafeCell,
    marker::PhantomData,
    mem::MaybeUninit,
    ptr::{self, NonNull},
    sync::atomic::{AtomicPtr, Ordering},
};

use ax_cpu_local::{CpuIndex, CpuPin};
use ax_kspin::PreemptGuard;
#[cfg(not(test))]
use ax_kspin::SpinNoIrq as Mutex;
#[cfg(test)]
use ax_kspin::{RawContext, RawSpinLock, SpinMutex};
use ax_percpu::BoundCpuPin;
use axvm_types::{
    GuestPhysAddr, InterruptTriggerMode, NestedPagingConfig, VCpuId, VMId, VmArchPerCpuOps,
    VmArchVcpuOps, VmBackendError, VmVcpuState,
};

use crate::{
    AxVmError, AxVmResult, ax_err,
    current_vcpu::{CurrentVcpuHeader, CurrentVcpuIdentity, CurrentVcpuInterruptError},
};

#[cfg(test)]
type Mutex<T> = SpinMutex<RawSpinLock<RawContext>, T>;

/// Borrowed proof that an AxVM operation cannot migrate between host CPUs.
///
/// The context does not disable preemption itself. Its only safe constructor
/// borrows the [`CpuPin`] owned by an outer host context guard, so backend
/// entry cannot outlive that guard.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PinnedCpuContext<'pin> {
    cpu_pin: &'pin CpuPin,
    bound_cpu_pin: BoundCpuPin<'pin>,
    identity: HostCpuIdentity,
}

impl<'pin> PinnedCpuContext<'pin> {
    /// Borrows the CPU pin owned by the caller's context guard.
    pub(crate) fn new(cpu_pin: &'pin CpuPin) -> Self {
        let bound_cpu_pin =
            ax_percpu::bound_current(cpu_pin).expect("vCPU entry requires a bound CPU-local area");
        Self {
            cpu_pin,
            identity: HostCpuIdentity::current(&bound_cpu_pin),
            bound_cpu_pin,
        }
    }

    /// Borrows the underlying CPU-local access proof.
    pub(crate) const fn cpu_pin(&self) -> &CpuPin {
        self.cpu_pin
    }

    /// Borrows the verified CPU-local area covered by the migration pin.
    pub(crate) const fn bound_cpu_pin(&self) -> &BoundCpuPin<'pin> {
        &self.bound_cpu_pin
    }

    /// Returns the logical index published by the pinned CPU-area header.
    pub(crate) const fn cpu_index(&self) -> CpuIndex {
        self.identity.cpu_index
    }

    /// Returns the pinned CPU index in the form expected by host backends.
    pub(crate) const fn cpu_index_usize(&self) -> usize {
        self.cpu_index().as_usize()
    }

    /// Returns the immutable CPU-area identity covered by this pin.
    const fn identity(&self) -> HostCpuIdentity {
        self.identity
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct HostCpuIdentity {
    cpu_index: CpuIndex,
    area_base: usize,
    generation: u32,
    cookie: usize,
}

impl HostCpuIdentity {
    fn current(cpu_pin: &BoundCpuPin<'_>) -> Self {
        let area = cpu_pin.area();
        Self {
            cpu_index: cpu_pin.cpu_index(),
            area_base: area.runtime_base(),
            generation: cpu_pin.generation(),
            cookie: cpu_pin.cookie(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum VcpuLifecycleState {
    Invalid,
    Created,
    Initializing,
    Free,
    Binding(HostCpuIdentity),
    Bound(HostCpuIdentity),
    Running(HostCpuIdentity),
    Unbinding(HostCpuIdentity),
}

impl VcpuLifecycleState {
    const fn public_state(self) -> VmVcpuState {
        match self {
            Self::Invalid => VmVcpuState::Invalid,
            Self::Created | Self::Initializing => VmVcpuState::Created,
            Self::Free => VmVcpuState::Free,
            Self::Binding(_) | Self::Bound(_) | Self::Unbinding(_) => VmVcpuState::Ready,
            Self::Running(_) => VmVcpuState::Running,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BackendAccess {
    FreeOnly,
    FreeOrBoundOwner,
    BoundOwnerOnly,
}

/// Published current-vCPU state tied to one pinned host CPU.
///
/// Dropping this scope clears `CURRENT_VCPU` before the borrowed CPU pin can
/// expire, including error returns from bind, guest entry, or unbind.
pub(crate) struct CurrentVcpuScope<'vcpu, 'pin, A: VmArchVcpuOps> {
    vcpu: &'vcpu AxVCpu<A>,
    pinned_cpu: PinnedCpuContext<'pin>,
    owns_publication: bool,
    _not_send_or_sync: PhantomData<*mut ()>,
}

/// Mutable lifecycle state of a virtual CPU.
struct AxVCpuInnerMut {
    state: VcpuLifecycleState,
}

struct AxVCpuInnerConst {
    vm_id: VMId,
    vcpu_id: VCpuId,
    phys_cpu_set: Option<usize>,
}

/// AxVM-owned architecture-independent vCPU wrapper.
pub struct AxVCpu<A: VmArchVcpuOps> {
    inner_const: AxVCpuInnerConst,
    inner_mut: Mutex<AxVCpuInnerMut>,
    current_header: CurrentVcpuHeader,
    arch_vcpu: UnsafeCell<A>,
}

/// Proof that one vCPU backend is bound to the current pinned CPU owner.
///
/// Only the pinned runner constructs this capability. Deferred task-context
/// work receives no `BoundVcpu`, so it cannot call live interrupt injection
/// after backend unbind.
pub(crate) struct BoundVcpu<'scope, 'cpu, A: VmArchVcpuOps> {
    vcpu: &'scope Arc<AxVCpu<A>>,
    pinned_cpu: &'scope PinnedCpuContext<'cpu>,
}

impl<'scope, 'cpu, A: VmArchVcpuOps> BoundVcpu<'scope, 'cpu, A> {
    pub(crate) fn new(
        vcpu: &'scope Arc<AxVCpu<A>>,
        pinned_cpu: &'scope PinnedCpuContext<'cpu>,
    ) -> Self {
        vcpu.assert_current_on_pinned_cpu(pinned_cpu);
        assert_eq!(
            vcpu.inner_mut.lock().state,
            VcpuLifecycleState::Bound(pinned_cpu.identity()),
            "bound vCPU capability requires the current owner lifecycle"
        );
        Self { vcpu, pinned_cpu }
    }

    pub(crate) fn id(&self) -> VCpuId {
        self.vcpu.id()
    }

    pub(crate) fn vm_id(&self) -> VMId {
        self.vcpu.vm_id()
    }

    pub(crate) fn with_arch_vcpu<T>(
        &self,
        operation: &'static str,
        use_arch_vcpu: impl for<'backend> FnOnce(&'backend mut A) -> T,
    ) -> AxVmResult<T> {
        self.vcpu.assert_current_on_pinned_cpu(self.pinned_cpu);
        self.vcpu
            .with_arch_vcpu_access(BackendAccess::BoundOwnerOnly, operation, use_arch_vcpu)
    }

    pub(crate) fn inject_interrupt(&self, vector: usize) -> AxVmResult {
        self.with_arch_vcpu("inject bound vCPU interrupt", |arch_vcpu| {
            arch_vcpu.inject_interrupt(vector)
        })?
        .map_err(|error| map_interrupt_backend_error("inject vCPU interrupt", error))
    }

    pub(crate) fn inject_interrupt_with_trigger(
        &self,
        vector: usize,
        trigger: InterruptTriggerMode,
    ) -> AxVmResult {
        self.with_arch_vcpu("inject triggered bound vCPU interrupt", |arch_vcpu| {
            arch_vcpu.inject_interrupt_with_trigger(vector, trigger)
        })?
        .map_err(|error| map_interrupt_backend_error("inject triggered vCPU interrupt", error))
    }

    pub(crate) fn drain_published_interrupts(&self) -> AxVmResult {
        self.with_arch_vcpu("drain current vCPU interrupt publications", |arch_vcpu| {
            self.vcpu
                .current_header
                .drain_pending(|vector| arch_vcpu.inject_interrupt(vector))
        })?
        .map_err(|error| map_interrupt_backend_error("inject current vCPU interrupt", error))
    }
}

impl<A: VmArchVcpuOps> AxVCpu<A> {
    /// Creates a new vCPU wrapper.
    pub fn new(
        vm_id: VMId,
        vcpu_id: VCpuId,
        phys_cpu_set: Option<usize>,
        arch_config: A::CreateConfig,
    ) -> AxVmResult<Self> {
        Ok(Self {
            inner_const: AxVCpuInnerConst {
                vm_id,
                vcpu_id,
                phys_cpu_set,
            },
            inner_mut: Mutex::new(AxVCpuInnerMut {
                state: VcpuLifecycleState::Created,
            }),
            current_header: CurrentVcpuHeader::new(vm_id, vcpu_id),
            arch_vcpu: UnsafeCell::new(
                A::new(vm_id, vcpu_id, arch_config)
                    .map_err(|error| map_vcpu_backend_error("create vCPU", error))?,
            ),
        })
    }

    /// Sets up this vCPU for execution.
    pub fn setup(
        &self,
        entry: GuestPhysAddr,
        nested_paging: NestedPagingConfig,
        arch_config: A::SetupConfig,
    ) -> AxVmResult {
        self.reserve_state(
            VcpuLifecycleState::Created,
            VcpuLifecycleState::Initializing,
        )?;
        // SAFETY: `Initializing` is an exclusive reservation installed before
        // the state lock was released. Every other safe backend entry rejects
        // that state.
        let result = (|| {
            // SAFETY: `Initializing` is the exclusive backend reservation for
            // this operation.
            let arch_vcpu = unsafe { self.arch_vcpu_mut_reserved() };
            arch_vcpu
                .set_entry(entry)
                .map_err(|error| map_vcpu_backend_error("set vCPU entry", error))?;
            arch_vcpu
                .set_nested_page_table(nested_paging)
                .map_err(|error| map_vcpu_backend_error("set nested page table", error))?;
            arch_vcpu
                .setup(arch_config)
                .map_err(|error| map_vcpu_backend_error("set up vCPU", error))?;
            Ok(())
        })();
        self.finish_reserved_state(
            VcpuLifecycleState::Initializing,
            if result.is_ok() {
                VcpuLifecycleState::Free
            } else {
                VcpuLifecycleState::Invalid
            },
        );
        result
    }

    /// Returns the vCPU id within its VM.
    pub const fn id(&self) -> VCpuId {
        self.inner_const.vcpu_id
    }

    /// Returns the VM id this vCPU belongs to.
    pub const fn vm_id(&self) -> VMId {
        self.inner_const.vm_id
    }

    /// Returns the allowed physical CPU mask.
    pub const fn phys_cpu_set(&self) -> Option<usize> {
        self.inner_const.phys_cpu_set
    }

    /// Returns the current vCPU state.
    pub fn state(&self) -> VmVcpuState {
        self.inner_mut.lock().state.public_state()
    }

    /// Publishes this vCPU as current for the lifetime of a pinned CPU scope.
    pub(crate) fn enter_pinned<'vcpu, 'pin>(
        &'vcpu self,
        pinned_cpu: &PinnedCpuContext<'pin>,
    ) -> CurrentVcpuScope<'vcpu, 'pin, A> {
        let current_header = load_current_vcpu_header(pinned_cpu);
        let owns_publication = match NonNull::new(current_header) {
            Some(current_header) => {
                assert!(
                    ptr::eq(
                        current_header.as_ptr(),
                        ptr::from_ref(&self.current_header).cast_mut()
                    ),
                    "nested vCPU operation is not allowed"
                );
                false
            }
            None => {
                set_current_vcpu_header(&self.current_header, pinned_cpu);
                true
            }
        };

        CurrentVcpuScope {
            vcpu: self,
            pinned_cpu: *pinned_cpu,
            owns_publication,
            _not_send_or_sync: PhantomData,
        }
    }

    /// Runs the vCPU until a VM exit.
    pub(crate) fn run<'cpu>(
        &'cpu self,
        pinned_cpu: &'cpu PinnedCpuContext<'_>,
    ) -> AxVmResult<A::Exit<'cpu>> {
        self.assert_current_on_pinned_cpu(pinned_cpu);
        let identity = pinned_cpu.identity();
        self.reserve_state(
            VcpuLifecycleState::Bound(identity),
            VcpuLifecycleState::Running(identity),
        )?;
        // SAFETY: `Running(identity)` exclusively reserves the backend until
        // the result is committed below.
        let result = unsafe { self.arch_vcpu_mut_reserved() }
            .run(pinned_cpu.cpu_pin())
            .map_err(|error| map_vcpu_backend_error("run vCPU", error));
        // VM entry errors still return through the architecture exit path. The
        // backend must remain bound so host-owned state can always be restored.
        self.finish_reserved_state(
            VcpuLifecycleState::Running(identity),
            VcpuLifecycleState::Bound(identity),
        );
        result
    }

    /// Binds the vCPU to the current physical CPU.
    pub(crate) fn bind(&self, pinned_cpu: &PinnedCpuContext<'_>) -> AxVmResult {
        self.assert_current_on_pinned_cpu(pinned_cpu);
        let identity = pinned_cpu.identity();
        self.reserve_state(
            VcpuLifecycleState::Free,
            VcpuLifecycleState::Binding(identity),
        )?;
        // SAFETY: `Binding(identity)` exclusively reserves the backend until
        // the result is committed below.
        let result = unsafe { self.arch_vcpu_mut_reserved() }
            .bind(pinned_cpu.cpu_pin())
            .map_err(|error| map_vcpu_backend_error("bind vCPU", error));
        self.finish_reserved_state(
            VcpuLifecycleState::Binding(identity),
            if result.is_ok() {
                VcpuLifecycleState::Bound(identity)
            } else {
                VcpuLifecycleState::Invalid
            },
        );
        result
    }

    /// Unbinds the vCPU from the current physical CPU.
    pub(crate) fn unbind(&self, pinned_cpu: &PinnedCpuContext<'_>) -> AxVmResult {
        self.assert_current_on_pinned_cpu(pinned_cpu);
        let identity = pinned_cpu.identity();
        self.reserve_state(
            VcpuLifecycleState::Bound(identity),
            VcpuLifecycleState::Unbinding(identity),
        )?;
        // SAFETY: `Unbinding(identity)` exclusively reserves the backend until
        // the result is committed below.
        let result = unsafe { self.arch_vcpu_mut_reserved() }
            .unbind(pinned_cpu.cpu_pin())
            .map_err(|error| map_vcpu_backend_error("unbind vCPU", error));
        self.finish_reserved_state(
            VcpuLifecycleState::Unbinding(identity),
            if result.is_ok() {
                VcpuLifecycleState::Free
            } else {
                VcpuLifecycleState::Invalid
            },
        );
        result
    }

    /// Sets the guest entry point.
    pub fn set_entry(&self, entry: GuestPhysAddr) -> AxVmResult {
        self.with_arch_vcpu_access(BackendAccess::FreeOnly, "set vCPU entry", |arch_vcpu| {
            arch_vcpu.set_entry(entry)
        })?
        .map_err(|error| map_vcpu_backend_error("set vCPU entry", error))
    }

    /// Sets a guest general-purpose register.
    pub fn set_gpr(&self, reg: usize, val: usize) {
        self.with_arch_vcpu("set vCPU general-purpose register", |arch_vcpu| {
            arch_vcpu.set_gpr(reg, val);
        })
        .expect("vCPU register update requires a free or owner-bound backend");
    }

    /// Sets the guest return value.
    pub fn set_return_value(&self, val: usize) {
        self.with_arch_vcpu("set vCPU return value", |arch_vcpu| {
            arch_vcpu.set_return_value(val);
        })
        .expect("vCPU return update requires a free or owner-bound backend");
    }

    /// Runs one short backend operation while the vCPU is free or owner-bound.
    ///
    /// The closure cannot return a backend borrow. It must not block, yield, or
    /// enter the guest because the lifecycle lock remains held for its call.
    pub(crate) fn with_arch_vcpu<T>(
        &self,
        operation: &'static str,
        use_arch_vcpu: impl for<'backend> FnOnce(&'backend mut A) -> T,
    ) -> AxVmResult<T> {
        self.with_arch_vcpu_access(BackendAccess::FreeOrBoundOwner, operation, use_arch_vcpu)
    }

    pub(crate) fn with_arch_vcpu_access<T>(
        &self,
        access: BackendAccess,
        operation: &'static str,
        use_arch_vcpu: impl for<'backend> FnOnce(&'backend mut A) -> T,
    ) -> AxVmResult<T> {
        let preempt_guard = PreemptGuard::new();
        let pinned_cpu = PinnedCpuContext::new(preempt_guard.cpu_pin());
        let inner_mut = self.inner_mut.lock();
        let access_allowed = match inner_mut.state {
            VcpuLifecycleState::Free => access != BackendAccess::BoundOwnerOnly,
            VcpuLifecycleState::Bound(owner)
                if access != BackendAccess::FreeOnly && owner == pinned_cpu.identity() =>
            {
                current_vcpu_matches(self, &pinned_cpu)
            }
            _ => false,
        };
        if !access_allowed {
            let current_state = inner_mut.state;
            drop(inner_mut);
            return ax_err!(
                BadState,
                format!("{operation} is unavailable for vCPU lifecycle {current_state:?}")
            );
        }

        // SAFETY: the lifecycle lock serializes short access while `Free`; an
        // owner-matched `Bound` state additionally proves this is the pinned
        // published owner. Reserved transition states are rejected above.
        Ok(use_arch_vcpu(unsafe { self.arch_vcpu_mut_reserved() }))
    }

    fn assert_current_on_pinned_cpu(&self, pinned_cpu: &PinnedCpuContext<'_>) {
        let _cpu_pin = pinned_cpu.cpu_pin();
        assert!(
            current_vcpu_matches(self, pinned_cpu),
            "vCPU backend entry requires a published pinned CPU context"
        );
    }

    fn reserve_state(
        &self,
        expected: VcpuLifecycleState,
        reserved: VcpuLifecycleState,
    ) -> AxVmResult {
        let mut inner_mut = self.inner_mut.lock();
        if inner_mut.state != expected {
            let current_state = inner_mut.state;
            drop(inner_mut);
            return ax_err!(
                BadState,
                format!("VCpu lifecycle is not {expected:?}, but {current_state:?}")
            );
        }
        inner_mut.state = reserved;
        Ok(())
    }

    fn finish_reserved_state(&self, reserved: VcpuLifecycleState, completed: VcpuLifecycleState) {
        let mut inner_mut = self.inner_mut.lock();
        assert_eq!(
            inner_mut.state, reserved,
            "vCPU lifecycle reservation changed while backend access was exclusive"
        );
        inner_mut.state = completed;
    }

    /// Returns the backend after the caller has reserved exclusive ownership.
    ///
    /// # Safety
    ///
    /// The caller must either own one of the lifecycle transition states or
    /// hold `inner_mut` in a state accepted by `with_arch_vcpu_access`. The
    /// returned borrow must end before that reservation or lock is released.
    #[allow(clippy::mut_from_ref)]
    unsafe fn arch_vcpu_mut_reserved(&self) -> &mut A {
        // SAFETY: the caller owns the backend exclusivity contract above.
        unsafe { &mut *self.arch_vcpu.get() }
    }
}

impl<A: VmArchVcpuOps> Drop for CurrentVcpuScope<'_, '_, A> {
    fn drop(&mut self) {
        if self.owns_publication {
            // Keep both borrows observably live until after the CPU-local value
            // is cleared; this prevents cleanup from drifting past the pin.
            let _pinned_cpu = self.pinned_cpu.cpu_pin();
            debug_assert!(
                current_vcpu_matches(self.vcpu, &self.pinned_cpu),
                "current vCPU changed inside one pinned scope"
            );
            clear_current_vcpu_header(&self.vcpu.current_header, &self.pinned_cpu);
        }
    }
}

#[ax_percpu::def_percpu]
static CURRENT_VCPU: AtomicPtr<CurrentVcpuHeader> = AtomicPtr::new(ptr::null_mut());

/// Runs a closure with the stable current-vCPU header on this physical CPU.
///
/// The higher-ranked closure prevents a safe caller from returning the
/// temporary reference beyond the CPU-local publication scope.
pub(crate) fn with_current_vcpu_header<T>(
    use_current: impl for<'current> FnOnce(Option<&'current CurrentVcpuHeader>) -> T,
) -> T {
    let preempt_guard = PreemptGuard::new();
    let pinned_cpu = PinnedCpuContext::new(preempt_guard.cpu_pin());
    with_current_vcpu_header_pinned(&pinned_cpu, use_current)
}

fn with_current_vcpu_header_pinned<T>(
    pinned_cpu: &PinnedCpuContext<'_>,
    use_current: impl for<'current> FnOnce(Option<&'current CurrentVcpuHeader>) -> T,
) -> T {
    let pointer = load_current_vcpu_header(pinned_cpu);
    // SAFETY: a non-null value is owned by a live CurrentVcpuScope on this
    // pinned CPU. The higher-ranked closure cannot safely return the temporary
    // borrow, and a same-CPU hard IRQ finishes before scope cleanup resumes.
    use_current(unsafe { NonNull::new(pointer).map(|pointer| pointer.as_ref()) })
}

fn current_vcpu_matches<A: VmArchVcpuOps>(
    vcpu: &AxVCpu<A>,
    pinned_cpu: &PinnedCpuContext<'_>,
) -> bool {
    load_current_vcpu_header(pinned_cpu) == ptr::from_ref(&vcpu.current_header).cast_mut()
}

/// Copies the identity of the vCPU published on this physical CPU.
pub(crate) fn current_vcpu_identity() -> Option<CurrentVcpuIdentity> {
    with_current_vcpu_header(|current| {
        current.map(|current| {
            let identity = current.identity();
            debug_assert_ne!(identity.generation(), 0);
            identity
        })
    })
}

/// Publishes an interrupt to the current vCPU's allocation-free IRQ header.
pub(crate) fn publish_current_vcpu_interrupt(
    vector: usize,
) -> Result<bool, CurrentVcpuInterruptError> {
    with_current_vcpu_header(|current| {
        let Some(current) = current else {
            return Ok(false);
        };
        current.publish_interrupt(vector)?;
        Ok(true)
    })
}

/// Sets the current AxVM vCPU header on this physical CPU.
fn set_current_vcpu_header(header: &CurrentVcpuHeader, pinned_cpu: &PinnedCpuContext<'_>) {
    header.begin_publication();
    let pointer = ptr::from_ref(header).cast_mut();
    // SAFETY: CpuPin keeps this per-CPU address stable for the atomic access.
    let slot = unsafe { &*CURRENT_VCPU.current_ptr(pinned_cpu.bound_cpu_pin()) };
    slot.compare_exchange(
        ptr::null_mut(),
        pointer,
        Ordering::AcqRel,
        Ordering::Acquire,
    )
    .expect("nested vCPU publication changed during pinned entry");
}

/// Clears the current AxVM vCPU header on this physical CPU.
fn clear_current_vcpu_header(header: &CurrentVcpuHeader, pinned_cpu: &PinnedCpuContext<'_>) {
    let pointer = ptr::from_ref(header).cast_mut();
    // SAFETY: CpuPin keeps this per-CPU address stable for the atomic access.
    let slot = unsafe { &*CURRENT_VCPU.current_ptr(pinned_cpu.bound_cpu_pin()) };
    slot.compare_exchange(
        pointer,
        ptr::null_mut(),
        Ordering::AcqRel,
        Ordering::Acquire,
    )
    .expect("current vCPU publication changed before pinned cleanup");
}

fn load_current_vcpu_header(pinned_cpu: &PinnedCpuContext<'_>) -> *mut CurrentVcpuHeader {
    // SAFETY: CpuPin keeps this per-CPU address stable for the atomic load. The
    // slot itself is atomic because a hard IRQ may inspect it on the same CPU.
    let slot = unsafe { &*CURRENT_VCPU.current_ptr(pinned_cpu.bound_cpu_pin()) };
    slot.load(Ordering::Acquire)
}

/// Host per-CPU virtualization state wrapper owned by AxVM.
pub struct AxPerCpu<A: VmArchPerCpuOps> {
    cpu_id: Option<usize>,
    arch: MaybeUninit<A>,
}

impl<A: VmArchPerCpuOps> AxPerCpu<A> {
    /// Creates an uninitialized per-CPU state.
    pub const fn new_uninit() -> Self {
        Self {
            cpu_id: None,
            arch: MaybeUninit::uninit(),
        }
    }

    /// Initializes this per-CPU state.
    pub fn init(&mut self, cpu_id: usize) -> AxVmResult {
        if self.cpu_id.is_some() {
            ax_err!(BadState, "per-CPU state is already initialized")
        } else {
            let arch = A::new(cpu_id).map_err(|error| {
                map_host_backend_error("initialize per-CPU virtualization", error)
            })?;
            self.arch.write(arch);
            self.cpu_id = Some(cpu_id);
            Ok(())
        }
    }

    /// Returns the initialized architecture state.
    pub fn arch_checked(&self) -> &A {
        assert!(self.cpu_id.is_some(), "per-CPU state is not initialized");
        unsafe { self.arch.assume_init_ref() }
    }

    /// Returns the initialized mutable architecture state.
    pub fn arch_checked_mut(&mut self) -> &mut A {
        assert!(self.cpu_id.is_some(), "per-CPU state is not initialized");
        unsafe { self.arch.assume_init_mut() }
    }

    /// Enables virtualization on the current CPU.
    pub fn hardware_enable(&mut self, pinned_cpu: &PinnedCpuContext<'_>) -> AxVmResult {
        self.arch_checked_mut()
            .hardware_enable(pinned_cpu.cpu_pin())
            .map_err(|error| map_host_backend_error("enable hardware virtualization", error))
    }
}

fn map_vcpu_backend_error(operation: &'static str, error: VmBackendError) -> AxVmError {
    match error {
        VmBackendError::InvalidInput => AxVmError::invalid_input(operation, error),
        VmBackendError::InvalidData => AxVmError::vcpu(operation, error),
        VmBackendError::InvalidState => AxVmError::invalid_state(operation, error),
        VmBackendError::Unsupported => AxVmError::unsupported(operation, error),
        VmBackendError::OutOfMemory => AxVmError::OutOfMemory { operation },
        VmBackendError::ResourceBusy => AxVmError::resource_conflict(
            "vCPU backend",
            format_args!("{operation} failed: {error}"),
        ),
    }
}

fn map_host_backend_error(operation: &'static str, error: VmBackendError) -> AxVmError {
    match error {
        VmBackendError::InvalidInput => AxVmError::invalid_input(operation, error),
        VmBackendError::InvalidData => AxVmError::host(operation, error),
        VmBackendError::InvalidState => AxVmError::invalid_state(operation, error),
        VmBackendError::Unsupported => AxVmError::unsupported(operation, error),
        VmBackendError::OutOfMemory => AxVmError::OutOfMemory { operation },
        VmBackendError::ResourceBusy => AxVmError::resource_conflict(
            "host virtualization backend",
            format_args!("{operation} failed: {error}"),
        ),
    }
}

pub(crate) fn map_interrupt_backend_error(
    operation: &'static str,
    error: VmBackendError,
) -> AxVmError {
    match error {
        VmBackendError::InvalidInput => AxVmError::invalid_input(operation, error),
        VmBackendError::InvalidData => AxVmError::interrupt(operation, error),
        VmBackendError::InvalidState => AxVmError::invalid_state(operation, error),
        VmBackendError::Unsupported => AxVmError::unsupported(operation, error),
        VmBackendError::OutOfMemory => AxVmError::OutOfMemory { operation },
        VmBackendError::ResourceBusy => AxVmError::resource_conflict(
            "interrupt backend",
            format_args!("{operation} failed: {error}"),
        ),
    }
}

#[cfg(test)]
mod tests {
    use alloc::sync::Arc;
    use core::{
        mem::ManuallyDrop,
        sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    };

    use ax_cpu_local::{
        CpuAreaPrefix, CpuIndex, CpuLocalAnchor, PerCpuRelocation, install_current,
    };
    use axvm_types::{HostPhysAddr, VmBackendResult};

    use super::*;

    #[derive(Default)]
    struct BackendTrace {
        bind_calls: AtomicUsize,
        run_calls: AtomicUsize,
        unbind_calls: AtomicUsize,
    }

    struct FailingRunBackend {
        trace: Arc<BackendTrace>,
    }

    static FAIL_NEXT_PERCPU_CONSTRUCTION: AtomicBool = AtomicBool::new(false);

    struct FalliblePerCpuBackend {
        enabled: bool,
    }

    impl VmArchPerCpuOps for FalliblePerCpuBackend {
        fn new(_cpu_id: usize) -> VmBackendResult<Self> {
            if FAIL_NEXT_PERCPU_CONSTRUCTION.swap(false, Ordering::AcqRel) {
                Err(VmBackendError::InvalidData)
            } else {
                Ok(Self { enabled: false })
            }
        }

        fn is_enabled(&self) -> bool {
            self.enabled
        }

        fn hardware_enable(&mut self, _cpu_pin: &CpuPin) -> VmBackendResult {
            self.enabled = true;
            Ok(())
        }

        fn hardware_disable(&mut self, _cpu_pin: &CpuPin) -> VmBackendResult {
            self.enabled = false;
            Ok(())
        }
    }

    impl VmArchVcpuOps for FailingRunBackend {
        type CreateConfig = Arc<BackendTrace>;
        type SetupConfig = ();
        type Exit<'cpu> = ();

        fn new(_vm_id: VMId, _vcpu_id: VCpuId, trace: Self::CreateConfig) -> VmBackendResult<Self> {
            Ok(Self { trace })
        }

        fn set_entry(&mut self, _entry: GuestPhysAddr) -> VmBackendResult {
            Ok(())
        }

        fn set_nested_page_table(&mut self, _config: NestedPagingConfig) -> VmBackendResult {
            Ok(())
        }

        fn setup(&mut self, _config: Self::SetupConfig) -> VmBackendResult {
            Ok(())
        }

        fn run<'cpu>(&'cpu mut self, _cpu_pin: &'cpu CpuPin) -> VmBackendResult<Self::Exit<'cpu>> {
            self.trace.run_calls.fetch_add(1, Ordering::Relaxed);
            Err(VmBackendError::InvalidState)
        }

        fn bind(&mut self, _cpu_pin: &CpuPin) -> VmBackendResult {
            self.trace.bind_calls.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }

        fn unbind(&mut self, _cpu_pin: &CpuPin) -> VmBackendResult {
            self.trace.unbind_calls.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }

        fn set_gpr(&mut self, _reg: usize, _val: usize) {}

        fn inject_interrupt(&mut self, _vector: usize) -> VmBackendResult {
            Ok(())
        }

        fn set_return_value(&mut self, _val: usize) {}
    }

    struct InstalledTestCpu {
        _prefix: alloc::boxed::Box<CpuAreaPrefix>,
        pin: CpuPin,
    }

    impl InstalledTestCpu {
        fn install() -> Self {
            let mut prefix = alloc::boxed::Box::new(CpuAreaPrefix::TEMPLATE);
            let runtime_base = (&raw mut *prefix).cast::<u8>() as usize;
            let anchor = CpuLocalAnchor::new(runtime_base, PerCpuRelocation::from_raw(0));
            let cpu_index = CpuIndex::try_from(0).unwrap();
            *prefix = CpuAreaPrefix::for_area(cpu_index, anchor, 1, 0xace0);
            // SAFETY: the boxed prefix stays mapped and exclusively owned by
            // this test, which is single-threaded and never enables migration.
            unsafe { install_current(anchor) };
            Self {
                _prefix: prefix,
                // SAFETY: this test never schedules or moves between CPUs.
                pin: unsafe { CpuPin::new_unchecked() },
            }
        }
    }

    #[test]
    fn failed_percpu_construction_is_transactional_and_retryable() {
        FAIL_NEXT_PERCPU_CONSTRUCTION.store(true, Ordering::Release);
        // ManuallyDrop keeps the RED assertion deterministic: the buggy state
        // must not run Drop after publishing an initialized marker without an
        // initialized backend value.
        let mut percpu = ManuallyDrop::new(AxPerCpu::<FalliblePerCpuBackend>::new_uninit());

        assert!(percpu.init(3).is_err());
        assert!(
            percpu.cpu_id.is_none(),
            "failed construction must not publish the initialized marker"
        );
        percpu
            .init(3)
            .expect("a failed construction must leave the per-CPU object retryable");
        assert_eq!(percpu.cpu_id, Some(3));
        drop(ManuallyDrop::into_inner(percpu));
    }

    #[test]
    fn run_error_still_unbinds_backend_before_leaving_pinned_scope() {
        let trace = Arc::new(BackendTrace::default());
        let vcpu = AxVCpu::<FailingRunBackend>::new(1, 0, None, Arc::clone(&trace)).unwrap();
        vcpu.setup(
            GuestPhysAddr::from(0x1000),
            NestedPagingConfig::new(HostPhysAddr::from(0x2000), 4, 48, 0),
            (),
        )
        .unwrap();
        let test_cpu = InstalledTestCpu::install();
        let pinned_cpu = PinnedCpuContext::new(&test_cpu.pin);
        let _current_vcpu = vcpu.enter_pinned(&pinned_cpu);

        vcpu.bind(&pinned_cpu).unwrap();
        assert!(vcpu.run(&pinned_cpu).is_err());
        vcpu.unbind(&pinned_cpu)
            .expect("run failure must leave the backend eligible for host cleanup");

        assert_eq!(trace.bind_calls.load(Ordering::Relaxed), 1);
        assert_eq!(trace.run_calls.load(Ordering::Relaxed), 1);
        assert_eq!(trace.unbind_calls.load(Ordering::Relaxed), 1);
        assert_eq!(vcpu.state(), VmVcpuState::Free);
    }

    #[test]
    fn binding_reservation_prevents_a_second_backend_owner() {
        let trace = Arc::new(BackendTrace::default());
        let vcpu = AxVCpu::<FailingRunBackend>::new(1, 0, None, Arc::clone(&trace)).unwrap();
        vcpu.setup(
            GuestPhysAddr::from(0x1000),
            NestedPagingConfig::new(HostPhysAddr::from(0x2000), 4, 48, 0),
            (),
        )
        .unwrap();
        let first_owner = HostCpuIdentity {
            cpu_index: CpuIndex::try_from(0).unwrap(),
            area_base: 0x1000,
            generation: 1,
            cookie: 0xace0,
        };
        let second_owner = HostCpuIdentity {
            cpu_index: CpuIndex::try_from(1).unwrap(),
            area_base: 0x2000,
            generation: 1,
            cookie: 0xace0,
        };

        vcpu.reserve_state(
            VcpuLifecycleState::Free,
            VcpuLifecycleState::Binding(first_owner),
        )
        .unwrap();

        assert!(
            vcpu.reserve_state(
                VcpuLifecycleState::Free,
                VcpuLifecycleState::Binding(second_owner),
            )
            .is_err()
        );
        assert_eq!(trace.bind_calls.load(Ordering::Relaxed), 0);
        assert_eq!(
            vcpu.inner_mut.lock().state,
            VcpuLifecycleState::Binding(first_owner)
        );
    }

    #[test]
    fn vcpu_backend_errors_keep_domain_context() {
        assert!(matches!(
            map_vcpu_backend_error("run vCPU", VmBackendError::InvalidState),
            AxVmError::InvalidState {
                operation: "run vCPU",
                ..
            }
        ));
        assert!(matches!(
            map_vcpu_backend_error("create vCPU", VmBackendError::OutOfMemory),
            AxVmError::OutOfMemory {
                operation: "create vCPU"
            }
        ));
        assert!(matches!(
            map_vcpu_backend_error("bind vCPU", VmBackendError::ResourceBusy),
            AxVmError::ResourceConflict {
                resource: "vCPU backend",
                ..
            }
        ));
    }

    #[test]
    fn host_backend_errors_keep_domain_context() {
        assert!(matches!(
            map_host_backend_error(
                "enable hardware virtualization",
                VmBackendError::Unsupported
            ),
            AxVmError::Unsupported {
                operation: "enable hardware virtualization",
                ..
            }
        ));
        assert!(matches!(
            map_host_backend_error(
                "initialize per-CPU virtualization",
                VmBackendError::InvalidData
            ),
            AxVmError::Host {
                operation: "initialize per-CPU virtualization",
                ..
            }
        ));
    }

    #[test]
    fn interrupt_backend_errors_keep_domain_context() {
        assert!(matches!(
            map_interrupt_backend_error("inject vCPU interrupt", VmBackendError::InvalidData),
            AxVmError::Interrupt {
                operation: "inject vCPU interrupt",
                ..
            }
        ));
        assert!(matches!(
            map_interrupt_backend_error("inject vCPU interrupt", VmBackendError::ResourceBusy),
            AxVmError::ResourceConflict {
                resource: "interrupt backend",
                ..
            }
        ));
    }
}
