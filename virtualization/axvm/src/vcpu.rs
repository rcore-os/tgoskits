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

use std::{
    cell::UnsafeCell,
    format,
    mem::MaybeUninit,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

use ax_std::os::arceos::{
    guard::PreemptGuard,
    percpu::{self as ax_percpu, CpuAreaRef, CpuPin},
    sync::IrqSafeMutex as Mutex,
};
use axvm_types::{
    GuestPhysAddr, InterruptTriggerMode, NestedPagingConfig, VCpuId, VMId, VmArchPerCpuOps,
    VmArchVcpuOps, VmBackendError, VmVcpuState,
};

use crate::{AxVmError, AxVmResult, ax_err};

/// Borrowed proof that one AxVM operation cannot migrate between host CPUs.
struct PinnedCpuContext<'pin, 'cpu> {
    cpu_pin: &'pin CpuPin<'cpu>,
    area: CpuAreaRef,
    #[cfg(feature = "tls")]
    kernel_tls: usize,
}

impl<'pin, 'cpu> PinnedCpuContext<'pin, 'cpu> {
    fn new(cpu_pin: &'pin CpuPin<'cpu>) -> Self {
        ax_std::os::arceos::percpu::current_area(cpu_pin)
            .expect("vCPU operation requires the installed per-CPU area");
        Self {
            cpu_pin,
            area: cpu_pin.area(),
            #[cfg(feature = "tls")]
            kernel_tls: cpu_local::kernel_tls(cpu_pin),
        }
    }

    fn assert_host_cpu_binding(&self) {
        // SAFETY: the outer NoPreempt guard remains active. A new pin forces a
        // fresh read of both host CPU-local and current-thread registers.
        let current = unsafe {
            ax_std::os::arceos::percpu::with_cpu_pin(|pin| {
                (
                    pin.area(),
                    #[cfg(feature = "tls")]
                    cpu_local::kernel_tls(pin),
                )
            })
        }
        .unwrap_or_else(|error| panic!("vCPU transition did not restore host state: {error}"));
        assert_eq!(
            current.0, self.area,
            "vCPU transition restored a different host CPU area"
        );
        #[cfg(feature = "tls")]
        assert_eq!(
            current.1, self.kernel_tls,
            "vCPU transition did not restore the host kernel TLS register"
        );
        assert_eq!(self.cpu_pin.area(), self.area);
    }
}

struct CurrentVcpuPublication<'scope, 'cpu> {
    pin: &'scope CpuPin<'cpu>,
}

impl Drop for CurrentVcpuPublication<'_, '_> {
    fn drop(&mut self) {
        CURRENT_VCPU.write_current(self.pin, 0);
    }
}

/// Mutable runtime state of a virtual CPU.
pub struct AxVCpuInnerMut {
    state: VmVcpuState,
}

struct AxVCpuInnerConst {
    vm_id: VMId,
    vcpu_id: VCpuId,
    phys_cpu_set: Option<usize>,
    guest_mpidr: Option<u64>,
}

const OUTSIDE_GUEST_MODE: usize = 0;
const EXITING_GUEST_MODE_BIT: usize = 1;

/// Architecture-independent ownership of one vCPU's guest-entry window.
///
/// The packed mode carries both the host CPU and whether an exit request has
/// already claimed the running guest. `exit_requested` is sticky across the
/// outside-guest window so a request racing the final entry check cannot be
/// lost.
pub(crate) struct VcpuRunState {
    mode: AtomicUsize,
    exit_requested: AtomicBool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg(target_arch = "x86_64")]
pub(crate) enum HardIrqExitClaim {
    OutsideGuest,
    LocalGuest,
    RemoteGuest,
    AlreadyClaimed,
}

impl VcpuRunState {
    const fn new() -> Self {
        Self {
            mode: AtomicUsize::new(OUTSIDE_GUEST_MODE),
            exit_requested: AtomicBool::new(false),
        }
    }

    fn guest_mode(cpu_id: usize) -> usize {
        cpu_id
            .checked_add(1)
            .and_then(|encoded| encoded.checked_shl(1))
            .expect("host CPU id does not fit the vCPU guest-mode publication")
    }

    fn guest_cpu(mode: usize) -> usize {
        (mode >> 1) - 1
    }

    fn enter(&self, cpu_id: usize) -> VcpuGuestEntry<'_> {
        let guest_mode = Self::guest_mode(cpu_id);
        self.mode
            .compare_exchange(
                OUTSIDE_GUEST_MODE,
                guest_mode,
                Ordering::Release,
                Ordering::Acquire,
            )
            .unwrap_or_else(|mode| {
                panic!("vCPU guest mode was not outside before entry: {mode:#x}")
            });
        VcpuGuestEntry {
            run_state: self,
            guest_mode,
        }
    }

    pub(crate) fn publish_exit_request(&self) {
        self.exit_requested.store(true, Ordering::Release);
    }

    /// Returns the remote CPU that must receive a guest-exit doorbell.
    pub(crate) fn request_exit(&self, current_cpu: usize) -> Option<usize> {
        loop {
            let mode = self.mode.load(Ordering::Acquire);
            if mode == OUTSIDE_GUEST_MODE || mode & EXITING_GUEST_MODE_BIT != 0 {
                return None;
            }
            let target_cpu = Self::guest_cpu(mode);
            if self
                .mode
                .compare_exchange(
                    mode,
                    mode | EXITING_GUEST_MODE_BIT,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
            {
                return (target_cpu != current_cpu).then_some(target_cpu);
            }
        }
    }

    /// Classifies the guest-exit work left after a local hard IRQ.
    #[cfg(target_arch = "x86_64")]
    pub(crate) fn claim_hard_irq_exit(&self, current_cpu: usize) -> HardIrqExitClaim {
        loop {
            let mode = self.mode.load(Ordering::Acquire);
            if mode == OUTSIDE_GUEST_MODE {
                return HardIrqExitClaim::OutsideGuest;
            }
            if mode & EXITING_GUEST_MODE_BIT != 0 {
                return HardIrqExitClaim::AlreadyClaimed;
            }
            if Self::guest_cpu(mode) != current_cpu {
                return HardIrqExitClaim::RemoteGuest;
            }
            if self
                .mode
                .compare_exchange(
                    mode,
                    mode | EXITING_GUEST_MODE_BIT,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
            {
                return HardIrqExitClaim::LocalGuest;
            }
        }
    }
}

struct VcpuGuestEntry<'state> {
    run_state: &'state VcpuRunState,
    guest_mode: usize,
}

impl VcpuGuestEntry<'_> {
    fn exit_requested(&self) -> bool {
        self.run_state.exit_requested.swap(false, Ordering::AcqRel)
            || self.run_state.mode.load(Ordering::Acquire) & EXITING_GUEST_MODE_BIT != 0
    }
}

impl Drop for VcpuGuestEntry<'_> {
    fn drop(&mut self) {
        let previous = self
            .run_state
            .mode
            .swap(OUTSIDE_GUEST_MODE, Ordering::Release);
        assert!(
            previous == self.guest_mode || previous == self.guest_mode | EXITING_GUEST_MODE_BIT,
            "vCPU guest-mode owner changed during one entry: {previous:#x}"
        );
    }
}

pub(crate) enum VcpuRunResult<E> {
    Retry,
    ExitRequested,
    VmExit(E),
}

#[allow(dead_code)]
fn reserve_cpu_on_state(state: &mut VmVcpuState) -> AxVmResult {
    if *state != VmVcpuState::Free {
        let current_state = *state;
        return ax_err!(
            BadState,
            format!("VCpu state is not Free, but {current_state:?}")
        );
    }
    *state = VmVcpuState::Starting;
    Ok(())
}

#[allow(dead_code)]
fn rollback_cpu_on_state(state: &mut VmVcpuState) {
    if *state == VmVcpuState::Starting {
        *state = VmVcpuState::Free;
    }
}

fn finish_cpu_on_start_state(state: &mut VmVcpuState, bind_succeeded: bool) -> AxVmResult {
    if *state != VmVcpuState::Starting {
        let current_state = *state;
        return ax_err!(
            BadState,
            format!("VCpu state is not Starting, but {current_state:?}")
        );
    }
    *state = if bind_succeeded {
        VmVcpuState::Ready
    } else {
        VmVcpuState::Free
    };
    Ok(())
}

fn cpu_off_state(state: &mut VmVcpuState) -> AxVmResult {
    if *state != VmVcpuState::Ready {
        let current_state = *state;
        return ax_err!(
            BadState,
            format!("VCpu state is not Ready, but {current_state:?}")
        );
    }
    *state = VmVcpuState::Free;
    Ok(())
}

/// AxVM-owned architecture-independent vCPU wrapper.
pub struct AxVCpu<A: VmArchVcpuOps> {
    inner_const: AxVCpuInnerConst,
    inner_mut: Mutex<AxVCpuInnerMut>,
    run_state: Arc<VcpuRunState>,
    arch_vcpu: UnsafeCell<A>,
}

impl<A: VmArchVcpuOps> AxVCpu<A> {
    /// Creates a new vCPU wrapper.
    pub fn new(
        vm_id: VMId,
        vcpu_id: VCpuId,
        phys_cpu_set: Option<usize>,
        arch_config: A::CreateConfig,
    ) -> AxVmResult<Self> {
        let guest_mpidr = A::guest_mpidr_from_create_config(&arch_config);
        Ok(Self {
            inner_const: AxVCpuInnerConst {
                vm_id,
                vcpu_id,
                phys_cpu_set,
                guest_mpidr,
            },
            inner_mut: Mutex::new(AxVCpuInnerMut {
                state: VmVcpuState::Created,
            }),
            run_state: Arc::new(VcpuRunState::new()),
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
        self.manipulate_arch_vcpu(VmVcpuState::Created, VmVcpuState::Free, |arch_vcpu| {
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
        })
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

    /// Returns the guest-visible MPIDR affinity for this vCPU, when the architecture has one.
    pub const fn guest_mpidr(&self) -> Option<u64> {
        self.inner_const.guest_mpidr
    }

    pub(crate) fn run_state(&self) -> Arc<VcpuRunState> {
        Arc::clone(&self.run_state)
    }

    /// Returns the current vCPU state.
    pub fn state(&self) -> VmVcpuState {
        self.inner_mut.lock().state
    }

    /// Reserves a free vCPU for PSCI CPU_ON.
    #[allow(dead_code)]
    pub(crate) fn reserve_for_cpu_on(&self) -> AxVmResult {
        let mut inner_mut = self.inner_mut.lock();
        reserve_cpu_on_state(&mut inner_mut.state)
    }

    /// Binds a CPU_ON-started vCPU and rolls it back to Free if bind fails.
    pub(crate) fn bind_after_cpu_on_or_rollback(&self) -> AxVmResult {
        {
            let inner_mut = self.inner_mut.lock();
            if inner_mut.state != VmVcpuState::Starting {
                let current_state = inner_mut.state;
                return ax_err!(
                    BadState,
                    format!("VCpu state is not Starting, but {current_state:?}")
                );
            }
        }

        finish_cpu_on_start_state(&mut self.inner_mut.lock().state, true)
    }

    /// Rolls a failed PSCI CPU_ON reservation back to Free.
    #[allow(dead_code)]
    pub(crate) fn rollback_cpu_on(&self) {
        let mut inner_mut = self.inner_mut.lock();
        rollback_cpu_on_state(&mut inner_mut.state);
    }

    /// Powers off a vCPU after PSCI CPU_OFF so it can be started again.
    pub(crate) fn power_off_after_cpu_off(&self) -> AxVmResult {
        let mut inner_mut = self.inner_mut.lock();
        cpu_off_state(&mut inner_mut.state)
    }

    /// Runs `f` if the current state equals `from`, then stores `to`.
    pub fn with_state_transition<F, T>(
        &self,
        from: VmVcpuState,
        to: VmVcpuState,
        f: F,
    ) -> AxVmResult<T>
    where
        F: FnOnce() -> AxVmResult<T>,
    {
        {
            let inner_mut = self.inner_mut.lock();
            if inner_mut.state != from {
                let current_state = inner_mut.state;
                return ax_err!(
                    BadState,
                    format!("VCpu state is not {from:?}, but {current_state:?}")
                );
            }
        }

        let result = f();
        self.inner_mut.lock().state = if result.is_err() {
            VmVcpuState::Invalid
        } else {
            to
        };
        result
    }

    /// Runs `f` with this vCPU recorded as current on the physical CPU.
    pub(crate) fn with_current_cpu_set<F, T>(&self, f: F) -> T
    where
        F: FnOnce() -> T,
    {
        let _guard = PreemptGuard::new();
        // SAFETY: the guard prevents migration through the backend operation,
        // guest run, restoration check, and publication withdrawal.
        unsafe {
            ax_std::os::arceos::percpu::with_cpu_pin(|cpu_pin| {
                let pinned_cpu = PinnedCpuContext::new(cpu_pin);

                if let Some(current_vcpu) = get_current_vcpu::<A>(cpu_pin) {
                    if std::ptr::eq(current_vcpu, self) {
                        let result = f();
                        pinned_cpu.assert_host_cpu_binding();
                        result
                    } else {
                        panic!("nested vCPU operation is not allowed");
                    }
                } else {
                    set_current_vcpu(self, cpu_pin);
                    let publication = CurrentVcpuPublication { pin: cpu_pin };
                    let result = f();
                    pinned_cpu.assert_host_cpu_binding();
                    drop(publication);
                    result
                }
            })
        }
        .expect("vCPU operation requires an installed CPU-local area")
    }

    /// Runs an architecture operation under a state transition.
    pub fn manipulate_arch_vcpu<F, T>(
        &self,
        from: VmVcpuState,
        to: VmVcpuState,
        f: F,
    ) -> AxVmResult<T>
    where
        F: FnOnce(&mut A) -> AxVmResult<T>,
    {
        self.with_state_transition(from, to, || {
            self.with_current_cpu_set(|| f(self.get_arch_vcpu()))
        })
    }

    /// Transitions the vCPU state without calling the architecture backend.
    pub fn transition_state(&self, from: VmVcpuState, to: VmVcpuState) -> AxVmResult {
        self.with_state_transition(from, to, || Ok(()))
    }

    /// Returns the architecture-specific vCPU.
    #[allow(clippy::mut_from_ref)]
    pub fn get_arch_vcpu(&self) -> &mut A {
        unsafe { &mut *self.arch_vcpu.get() }
    }

    /// Runs one already-loaded vCPU until a VM exit.
    ///
    /// The caller must keep [`Self::with_backend_bound_current_cpu`] active so
    /// the architecture backend remains loaded on one non-migrating host CPU,
    /// and must keep host IRQs disabled until this method returns. The x86 VMX
    /// backend uses that interval to switch host-owned syscall MSRs.
    pub(crate) fn run_loaded(
        &self,
        retry_before_entry: impl FnOnce() -> bool,
    ) -> AxVmResult<VcpuRunResult<A::Exit>> {
        self.transition_state(VmVcpuState::Ready, VmVcpuState::Running)?;
        self.with_state_transition(VmVcpuState::Running, VmVcpuState::Ready, || {
            let guest_entry = self.run_state.enter(crate::host::task::current_cpu_id());
            // Publish IN_GUEST before the final canonical-pending recheck.
            // A producer before this point is observed by the recheck; a
            // producer after it must observe IN_GUEST and request an exit.
            if retry_before_entry() {
                return Ok(VcpuRunResult::Retry);
            }
            if guest_entry.exit_requested() {
                return Ok(VcpuRunResult::ExitRequested);
            }
            let arch_vcpu = self.get_arch_vcpu();
            arch_vcpu
                .run()
                .map(VcpuRunResult::VmExit)
                .map_err(|error| map_vcpu_backend_error("run vCPU", error))
        })
    }

    /// Starts one logical vCPU run slice.
    ///
    /// Architecture CPU-local state is loaded separately for every guest
    /// entry by [`Self::with_backend_bound_current_cpu`].
    pub fn bind(&self) -> AxVmResult {
        self.transition_state(VmVcpuState::Free, VmVcpuState::Ready)
    }

    /// Finishes one logical vCPU run slice.
    pub fn unbind(&self) -> AxVmResult {
        self.transition_state(VmVcpuState::Ready, VmVcpuState::Free)
    }

    /// Loads architecture CPU-local state, runs `operation`, and unloads it
    /// before the CPU pin is released.
    ///
    /// This is AxVM's `vcpu_load()`/`vcpu_put()` boundary. Code that can block,
    /// allocate through a sleepable runtime, or invoke external device
    /// callbacks must execute after this method returns.
    pub(crate) fn with_backend_bound_current_cpu<F, T>(&self, operation: F) -> AxVmResult<T>
    where
        F: FnOnce() -> AxVmResult<T>,
    {
        self.with_current_cpu_set(|| {
            self.get_arch_vcpu()
                .bind()
                .map_err(|error| map_vcpu_backend_error("load vCPU on host CPU", error))?;

            let result = operation();
            let unload_result = self
                .get_arch_vcpu()
                .unbind()
                .map_err(|error| map_vcpu_backend_error("unload vCPU from host CPU", error));
            match result {
                Ok(value) => {
                    unload_result?;
                    Ok(value)
                }
                Err(error) => {
                    if let Err(unload_error) = unload_result {
                        warn!("vCPU unload after operation failure also failed: {unload_error:?}");
                    }
                    Err(error)
                }
            }
        })
    }

    /// Sets the guest entry point.
    #[allow(dead_code)]
    pub fn set_entry(&self, entry: GuestPhysAddr) -> AxVmResult {
        self.get_arch_vcpu()
            .set_entry(entry)
            .map_err(|error| map_vcpu_backend_error("set vCPU entry", error))
    }

    /// Sets a guest general-purpose register.
    pub fn set_gpr(&self, reg: usize, val: usize) {
        self.get_arch_vcpu().set_gpr(reg, val);
    }

    /// Injects an interrupt into the vCPU.
    pub fn inject_interrupt(&self, vector: usize) -> AxVmResult {
        self.get_arch_vcpu()
            .inject_interrupt(vector)
            .map_err(|error| map_interrupt_backend_error("inject vCPU interrupt", error))
    }

    /// Injects an interrupt while preserving its trigger-mode metadata.
    pub fn inject_interrupt_with_trigger(
        &self,
        vector: usize,
        trigger: InterruptTriggerMode,
    ) -> AxVmResult {
        self.get_arch_vcpu()
            .inject_interrupt_with_trigger(vector, trigger)
            .map_err(|error| map_interrupt_backend_error("inject vCPU interrupt", error))
    }

    /// Sets the guest return value.
    pub fn set_return_value(&self, val: usize) {
        self.get_arch_vcpu().set_return_value(val);
    }
}

#[ax_percpu::def_percpu]
static CURRENT_VCPU: usize = 0;

/// Gets the current AxVM vCPU on this physical CPU.
pub(crate) fn get_current_vcpu<'pin, A: VmArchVcpuOps>(
    pin: &'pin CpuPin<'_>,
) -> Option<&'pin AxVCpu<A>> {
    let pointer = CURRENT_VCPU.read_current(pin);
    // SAFETY: publication is scoped by with_current_cpu_set, which borrows the
    // live AxVCpu and clears this pointer before its CPU pin expires.
    unsafe { (pointer as *const AxVCpu<A>).as_ref() }
}

fn set_current_vcpu<A: VmArchVcpuOps>(vcpu: &AxVCpu<A>, pin: &CpuPin<'_>) {
    assert_eq!(
        CURRENT_VCPU.read_current(pin),
        0,
        "current vCPU publication must be empty"
    );
    CURRENT_VCPU.write_current(pin, vcpu as *const _ as usize);
}

/// Runs `operation` with the current vCPU borrowed only for a pinned CPU scope.
pub(crate) fn with_current_vcpu<A: VmArchVcpuOps, R>(
    operation: impl FnOnce(Option<&AxVCpu<A>>) -> R,
) -> R {
    let _guard = PreemptGuard::new();
    // SAFETY: the guard prevents migration through the closure.
    unsafe { ax_std::os::arceos::percpu::with_cpu_pin(|pin| operation(get_current_vcpu(pin))) }
        .expect("current vCPU lookup requires an installed CPU-local area")
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
            self.cpu_id = Some(cpu_id);
            self.arch.write(A::new(cpu_id).map_err(|error| {
                map_host_backend_error("initialize per-CPU virtualization", error)
            })?);
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

    /// Returns whether virtualization is enabled.
    pub fn is_enabled(&self) -> bool {
        self.arch_checked().is_enabled()
    }

    /// Enables virtualization on the current CPU.
    pub fn hardware_enable(&mut self) -> AxVmResult {
        self.arch_checked_mut()
            .hardware_enable()
            .map_err(|error| map_host_backend_error("enable hardware virtualization", error))
    }

    /// Disables virtualization on the current CPU.
    pub fn hardware_disable(&mut self) -> AxVmResult {
        self.arch_checked_mut()
            .hardware_disable()
            .map_err(|error| map_host_backend_error("disable hardware virtualization", error))
    }
}

impl<A: VmArchPerCpuOps> Drop for AxPerCpu<A> {
    fn drop(&mut self) {
        if self.cpu_id.is_some() && self.is_enabled() {
            self.hardware_disable().unwrap();
        }
    }
}

pub(crate) fn map_vcpu_backend_error(operation: &'static str, error: VmBackendError) -> AxVmError {
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

fn map_interrupt_backend_error(operation: &'static str, error: VmBackendError) -> AxVmError {
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
    use super::*;

    #[test]
    #[cfg(target_arch = "x86_64")]
    fn hard_irq_exit_claim_distinguishes_outside_local_and_remote_guest() {
        let state = VcpuRunState::new();

        assert_eq!(state.claim_hard_irq_exit(2), HardIrqExitClaim::OutsideGuest);

        let local_entry = state.enter(2);
        assert!(!local_entry.exit_requested());
        assert_eq!(state.claim_hard_irq_exit(2), HardIrqExitClaim::LocalGuest);
        drop(local_entry);

        let remote_entry = state.enter(5);
        assert!(!remote_entry.exit_requested());
        assert_eq!(state.claim_hard_irq_exit(2), HardIrqExitClaim::RemoteGuest);
        drop(remote_entry);
    }

    #[test]
    fn task_kick_claims_one_remote_guest_exit() {
        let state = VcpuRunState::new();
        let entry = state.enter(7);

        state.publish_exit_request();
        assert_eq!(state.request_exit(3), Some(7));
        assert_eq!(state.request_exit(3), None);

        drop(entry);
    }

    #[test]
    fn local_exit_claim_cancels_the_pending_guest_entry() {
        let state = VcpuRunState::new();
        let entry = state.enter(7);

        assert_eq!(state.request_exit(7), None);
        assert!(entry.exit_requested());

        drop(entry);
    }

    #[test]
    fn outside_guest_request_aborts_the_next_entry_once() {
        let state = VcpuRunState::new();
        state.publish_exit_request();

        let first = state.enter(1);
        assert!(first.exit_requested());
        drop(first);

        let second = state.enter(4);
        assert!(!second.exit_requested());
        drop(second);
    }

    #[test]
    fn vcpu_cpu_on_reservation_moves_free_to_starting() {
        let mut state = VmVcpuState::Free;

        reserve_cpu_on_state(&mut state).unwrap();

        assert_eq!(state, VmVcpuState::Starting);
        assert!(reserve_cpu_on_state(&mut state).is_err());
        assert_eq!(state, VmVcpuState::Starting);
    }

    #[test]
    fn vcpu_cpu_on_rollback_restores_starting_to_free() {
        let mut state = VmVcpuState::Starting;

        rollback_cpu_on_state(&mut state);

        assert_eq!(state, VmVcpuState::Free);
        rollback_cpu_on_state(&mut state);
        assert_eq!(state, VmVcpuState::Free);
    }

    #[test]
    fn vcpu_cpu_on_start_success_moves_starting_to_ready() {
        let mut state = VmVcpuState::Starting;

        finish_cpu_on_start_state(&mut state, true).unwrap();

        assert_eq!(state, VmVcpuState::Ready);
    }

    #[test]
    fn vcpu_cpu_on_start_failure_restores_starting_to_free() {
        let mut state = VmVcpuState::Starting;

        finish_cpu_on_start_state(&mut state, false).unwrap();

        assert_eq!(state, VmVcpuState::Free);
    }

    #[test]
    fn vcpu_cpu_off_returns_ready_to_free_for_reon() {
        let mut state = VmVcpuState::Free;

        reserve_cpu_on_state(&mut state).unwrap();
        assert_eq!(state, VmVcpuState::Starting);

        state = VmVcpuState::Ready;
        cpu_off_state(&mut state).unwrap();
        assert_eq!(state, VmVcpuState::Free);

        reserve_cpu_on_state(&mut state).unwrap();
        assert_eq!(state, VmVcpuState::Starting);
    }

    #[test]
    fn vcpu_cpu_off_rejects_non_ready_states() {
        for initial_state in [
            VmVcpuState::Created,
            VmVcpuState::Free,
            VmVcpuState::Starting,
            VmVcpuState::Running,
            VmVcpuState::Invalid,
        ] {
            let mut state = initial_state;

            assert!(cpu_off_state(&mut state).is_err());
            assert_eq!(state, initial_state);
        }
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
