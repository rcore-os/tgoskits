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

use alloc::format;
use core::{cell::UnsafeCell, mem::MaybeUninit};

use ax_kspin::SpinNoIrq as Mutex;
use axvm_types::{
    GuestPhysAddr, NestedPagingConfig, VCpuId, VMId, VmArchPerCpuOps, VmArchVcpuOps,
    VmBackendError, VmVcpuState,
};

use crate::{AxVmError, AxVmResult, ax_err};

/// Mutable runtime state of a virtual CPU.
pub struct AxVCpuInnerMut {
    state: VmVcpuState,
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
        Ok(Self {
            inner_const: AxVCpuInnerConst {
                vm_id,
                vcpu_id,
                phys_cpu_set,
            },
            inner_mut: Mutex::new(AxVCpuInnerMut {
                state: VmVcpuState::Created,
            }),
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

    /// Returns the current vCPU state.
    pub fn state(&self) -> VmVcpuState {
        self.inner_mut.lock().state
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
    pub fn with_current_cpu_set<F, T>(&self, f: F) -> T
    where
        F: FnOnce() -> T,
    {
        if let Some(current_vcpu) = get_current_vcpu::<A>() {
            if core::ptr::eq(current_vcpu, self) {
                f()
            } else {
                panic!("nested vCPU operation is not allowed");
            }
        } else {
            unsafe {
                set_current_vcpu(self);
            }
            let result = f();
            unsafe {
                clear_current_vcpu();
            }
            result
        }
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

    /// Runs the vCPU until a VM exit.
    pub fn run(&self) -> AxVmResult<A::Exit> {
        self.transition_state(VmVcpuState::Ready, VmVcpuState::Running)?;
        self.manipulate_arch_vcpu(VmVcpuState::Running, VmVcpuState::Ready, |arch_vcpu| {
            arch_vcpu
                .run()
                .map_err(|error| map_vcpu_backend_error("run vCPU", error))
        })
    }

    /// Binds the vCPU to the current physical CPU.
    pub fn bind(&self) -> AxVmResult {
        self.manipulate_arch_vcpu(VmVcpuState::Free, VmVcpuState::Ready, |arch_vcpu| {
            arch_vcpu
                .bind()
                .map_err(|error| map_vcpu_backend_error("bind vCPU", error))
        })
    }

    /// Unbinds the vCPU from the current physical CPU.
    pub fn unbind(&self) -> AxVmResult {
        self.manipulate_arch_vcpu(VmVcpuState::Ready, VmVcpuState::Free, |arch_vcpu| {
            arch_vcpu
                .unbind()
                .map_err(|error| map_vcpu_backend_error("unbind vCPU", error))
        })
    }

    /// Sets the guest entry point.
    #[expect(
        dead_code,
        reason = "only non-x86 guest firmware updates secondary vCPU entries"
    )]
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

    /// Sets the guest return value.
    pub fn set_return_value(&self, val: usize) {
        self.get_arch_vcpu().set_return_value(val);
    }
}

#[ax_percpu::def_percpu]
static mut CURRENT_VCPU: Option<*mut u8> = None;

/// Gets the current AxVM vCPU on this physical CPU.
#[allow(static_mut_refs)]
pub fn get_current_vcpu<'a, A: VmArchVcpuOps>() -> Option<&'a AxVCpu<A>> {
    unsafe {
        CURRENT_VCPU
            .current_ref_raw()
            .as_ref()
            .copied()
            .and_then(|p| (p as *const AxVCpu<A>).as_ref())
    }
}

/// Sets the current AxVM vCPU on this physical CPU.
///
/// # Safety
///
/// The caller must clear the current vCPU before the wrapped operation returns.
#[allow(static_mut_refs)]
pub unsafe fn set_current_vcpu<A: VmArchVcpuOps>(vcpu: &AxVCpu<A>) {
    unsafe {
        CURRENT_VCPU
            .current_ref_mut_raw()
            .replace(vcpu as *const _ as *mut u8);
    }
}

/// Clears the current AxVM vCPU on this physical CPU.
///
/// # Safety
///
/// The caller must only clear a vCPU it previously installed.
#[allow(static_mut_refs)]
pub unsafe fn clear_current_vcpu() {
    unsafe {
        CURRENT_VCPU.current_ref_mut_raw().take();
    }
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
