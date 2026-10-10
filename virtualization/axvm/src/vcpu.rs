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
    format,
    mem::MaybeUninit,
    ptr::{self, NonNull},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicPtr, AtomicUsize, Ordering},
    },
};

use ax_std::os::arceos::{
    guard::PreemptGuard,
    percpu::{self as ax_percpu, CpuAreaRef, CpuPin},
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

/// CPU-local publication describing the execution that owns this host CPU.
///
/// The value carries only the identity of the current execution and a fixed
/// signal target. It cannot reach `AxVCpu<A>` or the architecture backend: the
/// exclusive backend borrow lives on the owning scope's stack while this
/// publication is active, and the publication is cleared before that scope
/// returns.
pub(crate) struct ExecutionContext {
    vm_id: VMId,
    vcpu_id: VCpuId,
    signals: NonNull<VcpuSignals>,
}

impl ExecutionContext {
    /// Returns the VM id of the current execution.
    pub(crate) const fn vm_id(&self) -> VMId {
        self.vm_id
    }

    /// Returns the vCPU id of the current execution.
    pub(crate) const fn vcpu_id(&self) -> VCpuId {
        self.vcpu_id
    }

    /// Returns the fixed signal target bound to this execution.
    pub(crate) fn signals(&self) -> &VcpuSignals {
        // SAFETY: the publishing scope keeps the `Arc<VcpuSignals>` alive until
        // the CPU-local slot is cleared, and readers only observe the value on
        // the CPU that owns the publication.
        unsafe { self.signals.as_ref() }
    }

    /// Copies one RAM byte through the entry's pre-bound immutable decode view.
    /// This context cannot be cloned or escape the pinned publication scope.
    #[cfg(any(test, target_arch = "x86_64"))]
    pub(crate) fn read_guest_byte(&self, address: GuestPhysAddr) -> Option<u8> {
        let memory = self.signals().decode_memory.load(Ordering::Acquire);
        // SAFETY: this execution is borrowed from the current CPU's scoped
        // publication. DecodePublication retains the immutable RAM view until
        // the backend is unloaded and that publication has been cleared.
        unsafe { memory.as_ref() }?.read_byte(address).ok()
    }

    /// Whether the current execution still owns the guest-entry loop.
    #[cfg(target_arch = "aarch64")]
    pub(crate) fn entry_loop_is_active(&self) -> bool {
        self.signals().entry_loop_is_active()
    }
}

/// Clears the CPU-local execution publication when the exclusive scope exits.
struct CurrentExecutionPublication<'scope, 'cpu> {
    pin: &'scope CpuPin<'cpu>,
    _signals: Arc<VcpuSignals>,
}

impl<'scope, 'cpu> CurrentExecutionPublication<'scope, 'cpu> {
    fn publish(
        context: &ExecutionContext,
        signals: Arc<VcpuSignals>,
        pin: &'scope CpuPin<'cpu>,
    ) -> Self {
        assert_eq!(
            CURRENT_VCPU.read_current(pin),
            0,
            "current execution publication must be empty"
        );
        CURRENT_VCPU.write_current(pin, context as *const ExecutionContext as usize);
        Self {
            pin,
            _signals: signals,
        }
    }
}

impl Drop for CurrentExecutionPublication<'_, '_> {
    fn drop(&mut self) {
        CURRENT_VCPU.write_current(self.pin, 0);
    }
}

const OUTSIDE_GUEST_MODE: usize = 0;
const EXITING_GUEST_MODE_BIT: usize = 1;
const ENTRY_PARKED: usize = 0;
const ENTRY_OPEN: usize = 1;
const ENTRY_STOPPED: usize = 2;

/// Shared, architecture-independent signals for one vCPU execution.
///
/// The value is owned by an `Arc` shared with the runtime kick path and the
/// vCPU task. It contains only atomics: the guest-mode publication, the sticky
/// entry request, the logical unblock request, the stop request, and the
/// guest-entry-loop flag. It never contains or reaches the architecture
/// backend, and it exposes no upper-layer VM or lifecycle state.
///
/// `exit_requested` is sticky across the outside-guest window so a request
/// racing the final entry check cannot be lost. `stop_requested` additionally
/// closes the entry and park paths at once.
pub(crate) struct VcpuSignals {
    mode: AtomicUsize,
    exit_requested: AtomicBool,
    unblock_requested: AtomicBool,
    stop_requested: AtomicBool,
    admission: AtomicUsize,
    decode_memory: AtomicPtr<crate::guest_memory::DecodeMemory>,
    execution_target: AtomicPtr<crate::services::RunSignals>,
    entry_loop_active: AtomicBool,
}

impl VcpuSignals {
    pub(crate) const fn new() -> Self {
        Self {
            mode: AtomicUsize::new(OUTSIDE_GUEST_MODE),
            exit_requested: AtomicBool::new(false),
            unblock_requested: AtomicBool::new(false),
            stop_requested: AtomicBool::new(false),
            admission: AtomicUsize::new(ENTRY_PARKED),
            decode_memory: AtomicPtr::new(ptr::null_mut()),
            execution_target: AtomicPtr::new(ptr::null_mut()),
            entry_loop_active: AtomicBool::new(false),
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
        // Paired with the producer's fence in request_exit: either the final
        // request/admission check sees the publication or the producer sees
        // IN_GUEST and leaves a doorbell pending for this entry.
        std::sync::atomic::fence(Ordering::SeqCst);
        VcpuGuestEntry {
            signals: self,
            guest_mode,
        }
    }

    /// Publishes KVM_REQ_UNBLOCK-like work before waking this vCPU's thread.
    /// This is a wake request, not a second source of interrupt pending state.
    pub(crate) fn request_unblock(&self) {
        self.unblock_requested.store(true, Ordering::Release);
    }

    pub(crate) fn take_unblock_request(&self) -> bool {
        self.unblock_requested.swap(false, Ordering::AcqRel)
    }

    pub(crate) fn publish_exit_request(&self) {
        self.exit_requested.store(true, Ordering::Release);
    }

    /// Opens only a parked activation. A stopped activation cannot reopen.
    pub(crate) fn open_entry(&self) -> bool {
        let opened = self
            .admission
            .compare_exchange(
                ENTRY_PARKED,
                ENTRY_OPEN,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
            || self.admission.load(Ordering::Acquire) == ENTRY_OPEN;
        if opened {
            self.request_unblock();
        }
        opened
    }

    /// Closes admission before the owner publishes a park command and kicks.
    pub(crate) fn close_entry(&self) {
        let _ = self.admission.compare_exchange(
            ENTRY_OPEN,
            ENTRY_PARKED,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
        self.publish_exit_request();
        self.request_unblock();
    }

    /// An atomic-only predicate suitable for the host wait-queue boundary.
    pub(crate) fn entry_is_open(&self) -> bool {
        self.admission.load(Ordering::Acquire) == ENTRY_OPEN
    }

    /// Atomically closes the entry, park, and stop paths of this execution.
    ///
    /// A stop request stays published: the next entry publication observes the
    /// sticky exit request, a parked waiter observes the unblock request, and
    /// the task loop can retire towards `Stop`.
    pub(crate) fn request_stop(&self) {
        self.admission.store(ENTRY_STOPPED, Ordering::Release);
        self.stop_requested.store(true, Ordering::Release);
        self.exit_requested.store(true, Ordering::Release);
        self.unblock_requested.store(true, Ordering::Release);
    }

    /// Whether a stop has been requested for this execution.
    pub(crate) fn stop_requested(&self) -> bool {
        self.stop_requested.load(Ordering::Acquire)
    }

    /// Returns the remote CPU that must receive a guest-exit doorbell.
    pub(crate) fn request_exit(&self, current_cpu: usize) -> Option<usize> {
        // The caller publishes canonical pending/request state before this
        // fence. It pairs with the vCPU's publication and final entry check.
        std::sync::atomic::fence(Ordering::SeqCst);
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

    /// Claims the guest-entry loop for the current backend scope.
    ///
    /// Panics if the same execution already owns the loop, matching the
    /// prohibition on nested guest-entry scopes.
    fn begin_entry_loop(&self) {
        assert!(
            !self.entry_loop_active.swap(true, Ordering::AcqRel),
            "nested vCPU entry loop"
        );
    }

    fn end_entry_loop(&self) {
        self.entry_loop_active.store(false, Ordering::Release);
    }

    /// Whether this execution currently owns the guest-entry loop.
    #[cfg(target_arch = "aarch64")]
    pub(crate) fn entry_loop_is_active(&self) -> bool {
        self.entry_loop_active.load(Ordering::Acquire)
    }
}

struct VcpuGuestEntry<'signals> {
    signals: &'signals VcpuSignals,
    guest_mode: usize,
}

impl VcpuGuestEntry<'_> {
    fn exit_requested(&self) -> bool {
        !self.signals.entry_is_open()
            || self.signals.stop_requested()
            || self.signals.exit_requested.swap(false, Ordering::AcqRel)
            || self.signals.mode.load(Ordering::Acquire) & EXITING_GUEST_MODE_BIT != 0
    }
}

impl Drop for VcpuGuestEntry<'_> {
    fn drop(&mut self) {
        let previous = self
            .signals
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

/// AxVM-owned architecture-independent vCPU wrapper.
///
/// The wrapper exclusively owns the architecture backend `A` and the plain
/// `VmVcpuState`. Shared kick state lives only in the `Arc<VcpuSignals>`; the
/// architecture backend is never published and cannot be observed or mutated
/// through a shared reference. Every backend-touching operation therefore takes
/// `&mut self`.
pub struct AxVCpu<A: VmArchVcpuOps> {
    inner_const: AxVCpuInnerConst,
    state: VmVcpuState,
    run_state: Arc<VcpuSignals>,
    arch_vcpu: A,
}

struct AxVCpuInnerConst {
    vm_id: VMId,
    vcpu_id: VCpuId,
    phys_cpu_set: Option<usize>,
    guest_mpidr: Option<u64>,
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
            state: VmVcpuState::Created,
            run_state: Arc::new(VcpuSignals::new()),
            arch_vcpu: A::new(vm_id, vcpu_id, arch_config)
                .map_err(|error| map_vcpu_backend_error("create vCPU", error))?,
        })
    }

    /// Sets up this vCPU for execution.
    pub fn setup(
        &mut self,
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

    /// Returns the shared signal target used by the runtime kick path.
    pub(crate) fn run_state(&self) -> Arc<VcpuSignals> {
        Arc::clone(&self.run_state)
    }

    /// Rebinds a retired backend to one fresh activation's fixed signal target.
    pub(crate) fn replace_signals(&mut self, signals: Arc<VcpuSignals>) -> AxVmResult {
        if self.state != VmVcpuState::Free {
            return Err(AxVmError::invalid_state(
                "replace vCPU signals",
                "backend is not retired",
            ));
        }
        self.run_state = signals;
        Ok(())
    }

    /// Returns the current vCPU state.
    pub fn state(&self) -> VmVcpuState {
        self.state
    }

    /// Reserves a free vCPU for PSCI CPU_ON.
    pub(crate) fn reserve_for_cpu_on(&mut self) -> AxVmResult {
        reserve_cpu_on_state(&mut self.state)
    }

    /// Binds a CPU_ON-started vCPU and rolls it back to Free if bind fails.
    pub(crate) fn bind_after_cpu_on_or_rollback(&mut self) -> AxVmResult {
        if self.state != VmVcpuState::Starting {
            let current_state = self.state;
            return ax_err!(
                BadState,
                format!("VCpu state is not Starting, but {current_state:?}")
            );
        }
        finish_cpu_on_start_state(&mut self.state, true)
    }

    /// Rolls a failed PSCI CPU_ON reservation back to Free.
    pub(crate) fn rollback_cpu_on(&mut self) {
        rollback_cpu_on_state(&mut self.state);
    }

    /// Runs `f` if the current state equals `from`, then stores `to`.
    ///
    /// On failure the vCPU is parked in [`VmVcpuState::Invalid`]; the caller must
    /// re-create it rather than retry.
    pub fn with_state_transition<T>(
        &mut self,
        from: VmVcpuState,
        to: VmVcpuState,
        f: impl FnOnce(&mut Self) -> AxVmResult<T>,
    ) -> AxVmResult<T> {
        if self.state != from {
            return Err(AxVmError::VcpuState {
                expected: from,
                actual: self.state,
            });
        }

        let result = f(self);
        self.state = if result.is_err() {
            VmVcpuState::Invalid
        } else {
            to
        };
        result
    }

    /// Runs `operation` while this execution exclusively owns the host CPU.
    ///
    /// The scope pins the host CPU, publishes the identity-only
    /// [`ExecutionContext`] for CPU-local lookups, runs the operation, verifies
    /// that the architecture transition restored the host CPU binding, and
    /// clears the publication. The backend is borrowed exclusively for the
    /// whole scope, so no observer can reach `A` through the publication.
    pub(crate) fn with_exclusive_scope<T>(&mut self, operation: impl FnOnce(&mut Self) -> T) -> T {
        let _guard = PreemptGuard::new();
        // SAFETY: the guard prevents migration through the closure, the
        // publication withdrawal, and the host CPU binding check.
        unsafe {
            ax_std::os::arceos::percpu::with_cpu_pin(|cpu_pin| {
                let pinned_cpu = PinnedCpuContext::new(cpu_pin);
                let signals = Arc::clone(&self.run_state);

                if let Some(current) = get_current_execution(cpu_pin) {
                    if ptr::eq(current.signals(), &*signals) {
                        let result = operation(self);
                        pinned_cpu.assert_host_cpu_binding();
                        result
                    } else {
                        panic!("nested vCPU operation is not allowed");
                    }
                } else {
                    let context = ExecutionContext {
                        vm_id: self.inner_const.vm_id,
                        vcpu_id: self.inner_const.vcpu_id,
                        signals: NonNull::from(&*signals),
                    };
                    let publication =
                        CurrentExecutionPublication::publish(&context, signals, cpu_pin);
                    let result = operation(self);
                    pinned_cpu.assert_host_cpu_binding();
                    drop(publication);
                    result
                }
            })
        }
        .expect("vCPU operation requires an installed CPU-local area")
    }

    /// Runs an architecture operation under a state transition.
    pub fn manipulate_arch_vcpu<T>(
        &mut self,
        from: VmVcpuState,
        to: VmVcpuState,
        f: impl FnOnce(&mut A) -> AxVmResult<T>,
    ) -> AxVmResult<T> {
        self.with_state_transition(from, to, |vcpu| {
            vcpu.with_exclusive_scope(|vcpu| f(&mut vcpu.arch_vcpu))
        })
    }

    /// Transitions the vCPU state without calling the architecture backend.
    pub fn transition_state(&mut self, from: VmVcpuState, to: VmVcpuState) -> AxVmResult {
        self.with_state_transition(from, to, |_| Ok(()))
    }

    /// Runs an owner operation on the architecture backend.
    ///
    /// The backend is intentionally exposed only for the duration of this
    /// closure. AxVM owns the surrounding state transition, CPU pin and IRQ
    /// boundary; architecture code receives only its exclusive mutable value.
    pub(crate) fn with_backend<T>(&mut self, operation: impl FnOnce(&mut A) -> T) -> T {
        operation(&mut self.arch_vcpu)
    }

    /// Installs a nested page-table configuration through the owner boundary.
    pub(crate) fn set_nested_page_table(&mut self, config: NestedPagingConfig) -> AxVmResult {
        self.with_backend(|backend| {
            backend
                .set_nested_page_table(config)
                .map_err(|error| map_vcpu_backend_error("set nested page table", error))
        })
    }

    /// Runs one already-loaded vCPU until a VM exit.
    ///
    /// The caller must keep [`Self::with_backend_bound_current_cpu`] active so
    /// the architecture backend remains loaded on one non-migrating host CPU,
    /// and must keep host IRQs disabled until this method returns. The x86 VMX
    /// backend uses that interval to switch host-owned syscall MSRs.
    pub(crate) fn run_loaded(
        &mut self,
        retry_before_entry: impl FnOnce() -> bool,
    ) -> AxVmResult<VcpuRunResult<A::Exit>> {
        self.transition_state(VmVcpuState::Ready, VmVcpuState::Running)?;
        self.with_state_transition(VmVcpuState::Running, VmVcpuState::Ready, |vcpu| {
            let guest_entry = vcpu.run_state.enter(crate::host::task::current_cpu_id());
            // Publish IN_GUEST before the final canonical-pending recheck.
            // A producer before this point is observed by the recheck; a
            // producer after it must observe IN_GUEST and request an exit.
            if retry_before_entry() {
                return Ok(VcpuRunResult::Retry);
            }
            if guest_entry.exit_requested() {
                return Ok(VcpuRunResult::ExitRequested);
            }
            vcpu.arch_vcpu
                .run()
                .map(VcpuRunResult::VmExit)
                .map_err(|error| map_vcpu_backend_error("run vCPU", error))
        })
    }

    /// Starts one logical vCPU run slice.
    ///
    /// Architecture CPU-local state is loaded separately for every guest
    /// entry by [`Self::with_backend_bound_current_cpu`].
    pub fn bind(&mut self) -> AxVmResult {
        self.transition_state(VmVcpuState::Free, VmVcpuState::Ready)
    }

    /// Finishes one logical vCPU run slice.
    pub fn unbind(&mut self) -> AxVmResult {
        self.transition_state(VmVcpuState::Ready, VmVcpuState::Free)
    }

    /// Loads architecture CPU-local state, runs `operation`, and unloads it
    /// before the CPU pin is released.
    ///
    /// This is AxVM's `vcpu_load()`/`vcpu_put()` boundary. Code that can block,
    /// allocate through a sleepable runtime, or invoke external device
    /// callbacks must execute after this method returns.
    pub(crate) fn with_backend_bound_current_cpu<T>(
        &mut self,
        operation: impl FnOnce(&mut Self) -> AxVmResult<T>,
    ) -> AxVmResult<T> {
        self.with_exclusive_scope(|vcpu| {
            let signals = Arc::clone(&vcpu.run_state);
            let _entry_loop = EntryLoopGuard::new(&signals);
            let mut binding = BackendBinding::bind(vcpu)?;

            let result = binding.run(operation);
            if let Err(error) = binding.finish() {
                // A failed hardware retirement still owns this CPU and its
                // control leases. Returning or unwinding would release the pin
                // and permit migration with an active binding. This runtime
                // cannot recover that ownership transition; abort without
                // running the surrounding guard destructors.
                error!("cannot retire vCPU hardware on its binding CPU: {error:?}");
                std::process::abort();
            }
            result
        })
    }

    /// Runs the engine with an immutable decode view prepared in task context.
    pub(crate) fn with_engine_scope<T>(
        &mut self,
        memory: &crate::guest_memory::DecodeMemory,
        target: &crate::services::RunSignals,
        operation: impl FnOnce(&mut Self) -> AxVmResult<T>,
    ) -> AxVmResult<T> {
        let signals = Arc::clone(&self.run_state);
        let publication = DecodePublication::new(&signals, memory);
        let target_publication = ExecutionTargetPublication::new(&signals, target);
        let result = self.with_backend_bound_current_cpu(operation);
        drop(target_publication);
        drop(publication);
        result
    }

    /// Sets the guest entry point.
    pub fn set_entry(&mut self, entry: GuestPhysAddr) -> AxVmResult {
        self.arch_vcpu
            .set_entry(entry)
            .map_err(|error| map_vcpu_backend_error("set vCPU entry", error))
    }

    /// Sets a guest general-purpose register.
    pub fn set_gpr(&mut self, reg: usize, val: usize) {
        self.arch_vcpu.set_gpr(reg, val);
    }

    /// Injects an interrupt into the vCPU.
    pub fn inject_interrupt(&mut self, vector: usize) -> AxVmResult {
        self.arch_vcpu
            .inject_interrupt(vector)
            .map_err(|error| map_vcpu_backend_error("inject vCPU interrupt", error))
    }

    /// Injects an interrupt while preserving its trigger-mode metadata.
    pub fn inject_interrupt_with_trigger(
        &mut self,
        vector: usize,
        trigger: InterruptTriggerMode,
    ) -> AxVmResult {
        self.arch_vcpu
            .inject_interrupt_with_trigger(vector, trigger)
            .map_err(|error| map_vcpu_backend_error("inject vCPU interrupt", error))
    }

    /// Sets the guest return value.
    pub fn set_return_value(&mut self, val: usize) {
        self.arch_vcpu.set_return_value(val);
    }
}

/// The decode pointer is readable only through the current CPU's non-cloneable
/// ExecutionContext. The owner retains this borrowed view until that context
/// and its hardware binding have both been retired.
struct ExecutionTargetPublication<'scope> {
    signals: &'scope VcpuSignals,
    _target: &'scope crate::services::RunSignals,
}

impl<'scope> ExecutionTargetPublication<'scope> {
    fn new(signals: &'scope VcpuSignals, target: &'scope crate::services::RunSignals) -> Self {
        assert!(
            signals
                .execution_target
                .compare_exchange(
                    ptr::null_mut(),
                    ptr::from_ref(target).cast_mut(),
                    Ordering::Release,
                    Ordering::Relaxed,
                )
                .is_ok(),
            "nested execution signal publication"
        );
        Self {
            signals,
            _target: target,
        }
    }
}

impl Drop for ExecutionTargetPublication<'_> {
    fn drop(&mut self) {
        self.signals
            .execution_target
            .store(ptr::null_mut(), Ordering::Release);
    }
}

struct DecodePublication<'entry> {
    signals: &'entry VcpuSignals,
    _memory: &'entry crate::guest_memory::DecodeMemory,
}

impl<'entry> DecodePublication<'entry> {
    fn new(
        signals: &'entry VcpuSignals,
        memory: &'entry crate::guest_memory::DecodeMemory,
    ) -> Self {
        signals
            .decode_memory
            .compare_exchange(
                ptr::null_mut(),
                ptr::from_ref(memory).cast_mut(),
                Ordering::Release,
                Ordering::Acquire,
            )
            .expect("nested instruction-memory publication");
        Self {
            signals,
            _memory: memory,
        }
    }
}

impl Drop for DecodePublication<'_> {
    fn drop(&mut self) {
        self.signals
            .decode_memory
            .store(ptr::null_mut(), Ordering::Release);
    }
}

/// Owns one exclusive architecture backend binding on the current host CPU.
///
/// [`Self::bind`] loads the backend and [`Self::finish`] retires it on the
/// normal path. If the guest-entry operation panics, `Drop` retires the backend
/// before the surrounding CPU pin is released, so hardware ownership is never
/// left active on a migrating host CPU.
struct BackendBinding<'vcpu, A: VmArchVcpuOps> {
    vcpu: &'vcpu mut AxVCpu<A>,
    bound: bool,
}

impl<'vcpu, A: VmArchVcpuOps> BackendBinding<'vcpu, A> {
    fn bind(vcpu: &'vcpu mut AxVCpu<A>) -> AxVmResult<Self> {
        vcpu.arch_vcpu
            .bind()
            .map_err(|error| map_vcpu_backend_error("load vCPU on host CPU", error))?;
        Ok(Self { vcpu, bound: true })
    }

    fn run<R>(&mut self, operation: impl FnOnce(&mut AxVCpu<A>) -> R) -> R {
        operation(&mut *self.vcpu)
    }

    fn finish(mut self) -> AxVmResult {
        self.bound = false;
        self.vcpu
            .arch_vcpu
            .unbind()
            .map_err(|error| map_vcpu_backend_error("load vCPU on host CPU", error))
    }
}

impl<A: VmArchVcpuOps> Drop for BackendBinding<'_, A> {
    fn drop(&mut self) {
        if self.bound
            && let Err(error) = self.vcpu.arch_vcpu.unbind()
        {
            // Releasing the enclosing CPU pin after failed unloading could
            // migrate an active hardware binding. Retirement is unrecoverable.
            error!("vCPU unload during unwinding failed: {error:?}");
            std::process::abort();
        }
    }
}

#[ax_percpu::def_percpu]
static CURRENT_VCPU: usize = 0;

/// Owns the guest-entry-loop flag for one backend scope.
struct EntryLoopGuard<'signals> {
    signals: &'signals VcpuSignals,
}

impl<'signals> EntryLoopGuard<'signals> {
    fn new(signals: &'signals VcpuSignals) -> Self {
        signals.begin_entry_loop();
        Self { signals }
    }
}

impl Drop for EntryLoopGuard<'_> {
    fn drop(&mut self) {
        self.signals.end_entry_loop();
    }
}

/// Gets the current execution published on this physical CPU.
fn get_current_execution<'pin>(pin: &'pin CpuPin<'_>) -> Option<&'pin ExecutionContext> {
    let pointer = CURRENT_VCPU.read_current(pin);
    // SAFETY: publication is scoped by `with_exclusive_scope`, which keeps the
    // `ExecutionContext` and its `Arc<VcpuSignals>` alive and clears this slot
    // before the surrounding CPU pin expires.
    unsafe { (pointer as *const ExecutionContext).as_ref() }
}

/// Runs `operation` with the identity-only current execution.
///
/// The closure receives the current execution's identity and fixed signal
/// target, but never the architecture backend. The lookup is only valid for a
/// pinned CPU scope.
pub(crate) fn with_current_execution<R>(
    operation: impl FnOnce(Option<&ExecutionContext>) -> R,
) -> R {
    let _guard = PreemptGuard::new();
    // SAFETY: the guard prevents migration through the closure.
    unsafe { ax_std::os::arceos::percpu::with_cpu_pin(|pin| operation(get_current_execution(pin))) }
        .expect("current execution lookup requires an installed CPU-local area")
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
                map_vcpu_backend_error("initialize per-CPU virtualization", error)
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
            .map_err(|error| map_vcpu_backend_error("enable hardware virtualization", error))
    }

    /// Disables virtualization on the current CPU.
    pub fn hardware_disable(&mut self) -> AxVmResult {
        self.arch_checked_mut()
            .hardware_disable()
            .map_err(|error| map_vcpu_backend_error("disable hardware virtualization", error))
    }
}

impl<A: VmArchPerCpuOps> Drop for AxPerCpu<A> {
    fn drop(&mut self) {
        if self.cpu_id.is_some() && self.is_enabled() {
            self.hardware_disable().unwrap();
        }
    }
}

/// Keeps the machine failure owned and allocation-free until context restoration.
pub(crate) fn map_vcpu_backend_error(operation: &'static str, error: VmBackendError) -> AxVmError {
    match error {
        VmBackendError::OutOfMemory => AxVmError::OutOfMemory { operation },
        source => AxVmError::Backend { operation, source },
    }
}

#[cfg(test)]
mod tests {
    use std::{
        panic::{AssertUnwindSafe, catch_unwind},
        sync::atomic::{AtomicUsize, Ordering as AtomicOrdering},
    };

    use axvm_types::VmBackendResult;

    use super::*;

    #[derive(Default)]
    struct BindLog {
        binds: AtomicUsize,
        unbinds: AtomicUsize,
    }

    struct TestVcpu {
        log: Arc<BindLog>,
    }

    impl VmArchVcpuOps for TestVcpu {
        type CreateConfig = Arc<BindLog>;
        type SetupConfig = ();
        type Exit = ();

        fn new(_vm_id: VMId, _vcpu_id: VCpuId, log: Self::CreateConfig) -> VmBackendResult<Self> {
            Ok(Self { log })
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

        fn run(&mut self) -> VmBackendResult<Self::Exit> {
            Ok(())
        }

        fn bind(&mut self) -> VmBackendResult {
            self.log.binds.fetch_add(1, AtomicOrdering::SeqCst);
            Ok(())
        }

        fn unbind(&mut self) -> VmBackendResult {
            self.log.unbinds.fetch_add(1, AtomicOrdering::SeqCst);
            Ok(())
        }

        fn set_gpr(&mut self, _reg: usize, _val: usize) {}

        fn inject_interrupt(&mut self, _vector: usize) -> VmBackendResult {
            Ok(())
        }

        fn set_return_value(&mut self, _val: usize) {}
    }

    fn test_vcpu() -> (AxVCpu<TestVcpu>, Arc<BindLog>) {
        let log = Arc::new(BindLog::default());
        let vcpu = AxVCpu::<TestVcpu>::new(1, 0, None, Arc::clone(&log)).unwrap();
        (vcpu, log)
    }

    #[test]
    fn backend_binding_retires_exactly_once_on_the_normal_path() {
        let (mut vcpu, log) = test_vcpu();

        let mut binding = BackendBinding::bind(&mut vcpu).unwrap();
        let result: AxVmResult<u8> = binding.run(|_| Ok(7));
        binding.finish().unwrap();

        assert_eq!(result.unwrap(), 7);
        assert_eq!(log.binds.load(AtomicOrdering::SeqCst), 1);
        assert_eq!(log.unbinds.load(AtomicOrdering::SeqCst), 1);
    }

    #[test]
    fn backend_binding_unwinds_before_the_cpu_pin_is_released() {
        let (mut vcpu, log) = test_vcpu();

        let result = catch_unwind(AssertUnwindSafe(|| {
            let mut binding = BackendBinding::bind(&mut vcpu).unwrap();
            let _ = binding.run(|_| -> AxVmResult<()> { panic!("exit interpretation failed") });
        }));

        assert!(result.is_err());
        assert_eq!(log.binds.load(AtomicOrdering::SeqCst), 1);
        assert_eq!(
            log.unbinds.load(AtomicOrdering::SeqCst),
            1,
            "unwinding must release backend ownership"
        );
    }

    #[test]
    fn backend_binding_does_not_unbind_twice_after_finish() {
        let (mut vcpu, log) = test_vcpu();

        let binding = BackendBinding::bind(&mut vcpu).unwrap();
        binding.finish().unwrap();

        assert_eq!(log.binds.load(AtomicOrdering::SeqCst), 1);
        assert_eq!(log.unbinds.load(AtomicOrdering::SeqCst), 1);
    }

    #[test]
    fn task_kick_claims_one_remote_guest_exit() {
        let signals = VcpuSignals::new();
        assert!(signals.open_entry());
        let entry = signals.enter(7);

        signals.publish_exit_request();
        assert_eq!(signals.request_exit(3), Some(7));
        assert_eq!(signals.request_exit(3), None);

        drop(entry);
    }

    #[test]
    fn local_exit_claim_cancels_the_pending_guest_entry() {
        let signals = VcpuSignals::new();
        assert!(signals.open_entry());
        let entry = signals.enter(7);

        assert_eq!(signals.request_exit(7), None);
        assert!(entry.exit_requested());

        drop(entry);
    }

    #[test]
    fn outside_guest_request_aborts_the_next_entry_once() {
        let signals = VcpuSignals::new();
        assert!(signals.open_entry());
        signals.publish_exit_request();

        let first = signals.enter(1);
        assert!(first.exit_requested());
        drop(first);

        let second = signals.enter(4);
        assert!(!second.exit_requested());
        drop(second);
    }

    #[test]
    fn stop_request_closes_entry_park_and_stop_paths() {
        let signals = VcpuSignals::new();
        assert!(signals.open_entry());

        // A park after IN_GUEST publication must prevent the pending entry.
        let parked = signals.enter(1);
        signals.close_entry();
        assert!(parked.exit_requested());
        drop(parked);
        assert!(!signals.entry_is_open());
        assert!(signals.open_entry());

        signals.request_stop();

        assert!(signals.stop_requested());
        assert!(signals.take_unblock_request());
        let entry = signals.enter(1);
        assert!(entry.exit_requested());
        drop(entry);
        assert!(!signals.open_entry());
        // Consuming the one-shot request cannot reopen stopped admission.
        let late = signals.enter(1);
        assert!(late.exit_requested());
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
    fn vcpu_backend_errors_keep_domain_context() {
        assert!(matches!(
            map_vcpu_backend_error("run vCPU", VmBackendError::InvalidState),
            AxVmError::Backend {
                operation: "run vCPU",
                source: VmBackendError::InvalidState
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
            AxVmError::Backend {
                operation: "bind vCPU",
                source: VmBackendError::ResourceBusy
            }
        ));
    }

    #[test]
    fn host_backend_errors_keep_domain_context() {
        assert!(matches!(
            map_vcpu_backend_error(
                "enable hardware virtualization",
                VmBackendError::Unsupported
            ),
            AxVmError::Backend {
                operation: "enable hardware virtualization",
                source: VmBackendError::Unsupported
            }
        ));
        assert!(matches!(
            map_vcpu_backend_error(
                "initialize per-CPU virtualization",
                VmBackendError::InvalidData
            ),
            AxVmError::Backend {
                operation: "initialize per-CPU virtualization",
                source: VmBackendError::InvalidData
            }
        ));
    }

    #[test]
    fn interrupt_backend_errors_keep_domain_context() {
        assert!(matches!(
            map_vcpu_backend_error("inject vCPU interrupt", VmBackendError::InvalidData),
            AxVmError::Backend {
                operation: "inject vCPU interrupt",
                source: VmBackendError::InvalidData
            }
        ));
        assert!(matches!(
            map_vcpu_backend_error("inject vCPU interrupt", VmBackendError::ResourceBusy),
            AxVmError::Backend {
                operation: "inject vCPU interrupt",
                source: VmBackendError::ResourceBusy
            }
        ));
    }
}
