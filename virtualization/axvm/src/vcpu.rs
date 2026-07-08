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

use ax_errno::{AxResult, ax_err};
use ax_kspin::SpinNoIrq as Mutex;
#[cfg(target_arch = "x86_64")]
use axvm_types::InterruptTriggerMode;
use axvm_types::{
    GuestPhysAddr, NestedPagingConfig, VCpuId, VMId, VmArchPerCpuOps, VmArchVcpuOps, VmVcpuState,
};

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
    ) -> AxResult<Self> {
        Ok(Self {
            inner_const: AxVCpuInnerConst {
                vm_id,
                vcpu_id,
                phys_cpu_set,
            },
            inner_mut: Mutex::new(AxVCpuInnerMut {
                state: VmVcpuState::Created,
            }),
            arch_vcpu: UnsafeCell::new(A::new(vm_id, vcpu_id, arch_config)?),
        })
    }

    /// Sets up this vCPU for execution.
    pub fn setup(
        &self,
        entry: GuestPhysAddr,
        nested_paging: NestedPagingConfig,
        arch_config: A::SetupConfig,
    ) -> AxResult {
        self.manipulate_arch_vcpu(VmVcpuState::Created, VmVcpuState::Free, |arch_vcpu| {
            arch_vcpu.set_entry(entry)?;
            arch_vcpu.set_nested_page_table(nested_paging)?;
            arch_vcpu.setup(arch_config)?;
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
    ) -> AxResult<T>
    where
        F: FnOnce() -> AxResult<T>,
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
    ) -> AxResult<T>
    where
        F: FnOnce(&mut A) -> AxResult<T>,
    {
        self.with_state_transition(from, to, || {
            self.with_current_cpu_set(|| f(self.get_arch_vcpu()))
        })
    }

    /// Transitions the vCPU state without calling the architecture backend.
    pub fn transition_state(&self, from: VmVcpuState, to: VmVcpuState) -> AxResult {
        self.with_state_transition(from, to, || Ok(()))
    }

    /// Returns the architecture-specific vCPU.
    #[allow(clippy::mut_from_ref)]
    pub fn get_arch_vcpu(&self) -> &mut A {
        unsafe { &mut *self.arch_vcpu.get() }
    }

    /// Runs the vCPU until a VM exit.
    pub fn run(&self) -> AxResult<A::Exit> {
        self.transition_state(VmVcpuState::Ready, VmVcpuState::Running)?;
        self.manipulate_arch_vcpu(VmVcpuState::Running, VmVcpuState::Ready, |arch_vcpu| {
            arch_vcpu.run()
        })
    }

    /// Binds the vCPU to the current physical CPU.
    pub fn bind(&self) -> AxResult {
        self.manipulate_arch_vcpu(VmVcpuState::Free, VmVcpuState::Ready, |arch_vcpu| {
            arch_vcpu.bind()
        })
    }

    /// Unbinds the vCPU from the current physical CPU.
    pub fn unbind(&self) -> AxResult {
        self.manipulate_arch_vcpu(VmVcpuState::Ready, VmVcpuState::Free, |arch_vcpu| {
            arch_vcpu.unbind()
        })
    }

    /// Sets the guest entry point.
    pub fn set_entry(&self, entry: GuestPhysAddr) -> AxResult {
        self.get_arch_vcpu().set_entry(entry)
    }

    /// Sets a guest general-purpose register.
    pub fn set_gpr(&self, reg: usize, val: usize) {
        self.get_arch_vcpu().set_gpr(reg, val);
    }

    /// Injects an interrupt into the vCPU.
    pub fn inject_interrupt(&self, vector: usize) -> AxResult {
        self.get_arch_vcpu().inject_interrupt(vector)
    }

    /// Injects an interrupt with trigger-mode metadata.
    #[cfg(target_arch = "x86_64")]
    pub fn inject_interrupt_with_trigger(
        &self,
        vector: usize,
        trigger: InterruptTriggerMode,
    ) -> AxResult {
        self.get_arch_vcpu()
            .inject_interrupt_with_trigger(vector, trigger)
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
    pub fn init(&mut self, cpu_id: usize) -> AxResult {
        if self.cpu_id.is_some() {
            ax_err!(BadState, "per-CPU state is already initialized")
        } else {
            self.cpu_id = Some(cpu_id);
            self.arch.write(A::new(cpu_id)?);
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
    pub fn hardware_enable(&mut self) -> AxResult {
        self.arch_checked_mut().hardware_enable()
    }

    /// Disables virtualization on the current CPU.
    pub fn hardware_disable(&mut self) -> AxResult {
        self.arch_checked_mut().hardware_disable()
    }
}

impl<A: VmArchPerCpuOps> Drop for AxPerCpu<A> {
    fn drop(&mut self) {
        if self.cpu_id.is_some() && self.is_enabled() {
            self.hardware_disable().unwrap();
        }
    }
}
