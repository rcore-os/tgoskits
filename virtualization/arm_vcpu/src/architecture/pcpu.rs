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

use core::{marker::PhantomData, mem};

use aarch64_cpu::registers::*;

use crate::{
    ArmVcpuResult,
    enable::{El2DisableOperations, El2EnableOperations, HostHookSet, disable_el2, enable_el2},
    types::ArmHostOps,
};

/// Per-CPU AArch64 virtualization state.
#[repr(C)]
#[repr(align(4096))]
pub struct ArmPerCpu {
    /// per cpu id
    pub cpu_id: usize,
    /// The original value of `VBAR_EL2` (exception vector base) before enabling
    /// the virtualization.
    pub original_vbar_el2: u64,
    host_hooks: Option<HostHookSet>,
    timer_frequency_hz: u64,
}

unsafe extern "C" {
    fn exception_vector_base_vcpu();
}

struct HardwareEnable<H>(PhantomData<fn() -> H>);

impl<H: ArmHostOps> El2EnableOperations for HardwareEnable<H> {
    type HostHooks = HostHookSet;

    fn install_host_hooks(&mut self) -> ArmVcpuResult<Self::HostHooks> {
        super::host::install_host_hooks::<H>()
    }

    fn install_exception_vector(&mut self) {
        VBAR_EL2.set(exception_vector_base_vcpu as *const () as usize as _);
    }

    fn synchronize_context(&mut self) {
        synchronize_context();
    }

    fn enable_virtualization(&mut self) {
        HCR_EL2.modify(
            HCR_EL2::VM::Enable + HCR_EL2::RW::EL1IsAarch64 + HCR_EL2::TSC::EnableTrapEl1SmcToEl2,
        );
    }
}

struct HardwareDisable<'a> {
    original_vbar_el2: &'a mut u64,
    hooks: HostHookSet,
}

impl El2DisableOperations for HardwareDisable<'_> {
    fn validate_host_hooks(&mut self) -> ArmVcpuResult {
        super::host::validate_host_hook_release(self.hooks)
    }

    fn restore_exception_vector(&mut self) {
        VBAR_EL2.set(mem::take(self.original_vbar_el2));
    }

    fn synchronize_context(&mut self) {
        synchronize_context();
    }

    fn release_host_hooks(&mut self) {
        super::host::release_host_hooks(self.hooks);
    }

    fn disable_virtualization(&mut self) {
        HCR_EL2.set(HCR_EL2::VM::Disable.into());
    }
}

impl ArmPerCpu {
    /// Creates per-CPU virtualization state.
    pub fn new(cpu_id: usize) -> ArmVcpuResult<Self> {
        let timer_frequency_hz = CNTFRQ_EL0.get();
        if timer_frequency_hz == 0 {
            return Err(crate::ArmVcpuError::Unsupported);
        }
        Ok(Self {
            cpu_id,
            original_vbar_el2: 0,
            host_hooks: None,
            timer_frequency_hz,
        })
    }

    /// Returns whether AArch64 virtualization is enabled on the current CPU.
    pub fn is_enabled(&self) -> bool {
        HCR_EL2.is_set(HCR_EL2::VM)
    }

    /// Enables AArch64 virtualization on the current CPU.
    pub fn hardware_enable<H: ArmHostOps>(&mut self) -> ArmVcpuResult {
        if self.host_hooks.is_some() {
            return Err(crate::ArmVcpuError::BadState);
        }
        // First we save origin `exception_vector_base`.
        // Safety:
        // Todo: take care of `preemption`
        self.original_vbar_el2 = VBAR_EL2.get();

        let mut operations = HardwareEnable::<H>(PhantomData);
        self.host_hooks = Some(enable_el2(&mut operations)?);

        // Note that `ICH_HCR_EL2` is not the same as `HCR_EL2`.
        //
        // `ICH_HCR_EL2[0]` controls the virtual CPU interface operation.
        //
        // We leave it for the virtual GIC implementations to decide whether to enable it or not.
        //
        // unsafe {
        //     core::arch::asm! {
        //         "msr ich_hcr_el2, {value:x}",
        //         value = in(reg) 0,
        //     }
        // }

        Ok(())
    }

    /// Disables AArch64 virtualization on the current CPU.
    pub fn hardware_disable(&mut self) -> ArmVcpuResult {
        let hooks = self.host_hooks.ok_or(crate::ArmVcpuError::BadState)?;
        let mut operations = HardwareDisable {
            original_vbar_el2: &mut self.original_vbar_el2,
            hooks,
        };
        disable_el2(&mut operations)?;
        self.host_hooks = None;
        Ok(())
    }

    /// Returns the maximum guest page table levels supported by this CPU.
    pub fn max_guest_page_table_levels(&self) -> usize {
        super::vcpu::max_gpt_level(super::vcpu::pa_bits())
    }

    /// Returns the guest physical address width supported by this CPU.
    pub fn guest_phys_addr_bits(&self) -> usize {
        super::vcpu::pa_bits()
    }

    /// Returns the architectural counter frequency recorded on this CPU.
    pub const fn timer_frequency_hz(&self) -> u64 {
        self.timer_frequency_hz
    }
}

fn synchronize_context() {
    // SAFETY: `isb` only synchronizes subsequent instruction execution on the
    // current CPU after the system-register updates performed here.
    unsafe {
        core::arch::asm!("isb", options(nostack, preserves_flags));
    }
}
