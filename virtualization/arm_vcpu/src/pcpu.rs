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

use aarch64_cpu::registers::*;
use arm_gic_driver::v3::{ICH_HCR_EL2, ICH_VTR_EL2, Readable, Writeable, ich_lr_el2_set_raw};

use crate::{ArmHostOps, ArmVcpuResult, IchCapabilityProfile};

/// Per-CPU AArch64 virtualization state.
#[repr(C)]
#[repr(align(4096))]
pub struct ArmPerCpu {
    /// per cpu id
    pub cpu_id: usize,
    /// The original value of `VBAR_EL2` (exception vector base) before enabling
    /// the virtualization.
    pub original_vbar_el2: u64,
    /// Original hypervisor configuration restored by [`Self::hardware_disable`].
    pub original_hcr_el2: u64,
    enabled: bool,
}

unsafe extern "C" {
    fn exception_vector_base_vcpu();
}

impl ArmPerCpu {
    /// Creates per-CPU virtualization state.
    pub fn new(cpu_id: usize) -> ArmVcpuResult<Self> {
        Ok(Self {
            cpu_id,
            original_vbar_el2: 0,
            original_hcr_el2: 0,
            enabled: false,
        })
    }

    /// Returns whether AArch64 virtualization is enabled on the current CPU.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Enables AArch64 virtualization on the current CPU.
    pub fn hardware_enable<H: ArmHostOps>(&mut self) -> ArmVcpuResult {
        if self.enabled {
            return Ok(());
        }

        let profile = IchCapabilityProfile::from_raw_vtr(ICH_VTR_EL2.get())?;
        ensure_capability_can_be_published(self.cpu_id, profile)?;

        self.original_vbar_el2 = VBAR_EL2.get();
        self.original_hcr_el2 = HCR_EL2.get();

        disable_and_clear_ich(profile);
        crate::host::install_current_el_irq_handler::<H>();

        VBAR_EL2.set(exception_vector_base_vcpu as *const () as usize as _);
        HCR_EL2.modify(
            HCR_EL2::VM::Enable + HCR_EL2::RW::EL1IsAarch64 + HCR_EL2::TSC::EnableTrapEl1SmcToEl2,
        );

        if let Err(error) = crate::ich::publish_ich_capability(self.cpu_id, profile) {
            self.rollback_enable(profile);
            return Err(error);
        }

        self.enabled = true;
        Ok(())
    }

    /// Disables AArch64 virtualization on the current CPU.
    pub fn hardware_disable(&mut self) -> ArmVcpuResult {
        if !self.enabled {
            return Ok(());
        }

        let profile = crate::ich_capability(self.cpu_id)?;
        disable_and_clear_ich(profile);
        VBAR_EL2.set(self.original_vbar_el2);
        HCR_EL2.set(self.original_hcr_el2);
        crate::host::clear_current_el_irq_handler();

        self.original_vbar_el2 = 0;
        self.original_hcr_el2 = 0;
        self.enabled = false;
        Ok(())
    }

    /// Returns the maximum guest page table levels supported by this CPU.
    pub fn max_guest_page_table_levels(&self) -> usize {
        crate::vcpu::max_gpt_level(crate::vcpu::pa_bits())
    }

    /// Returns the guest physical address width supported by this CPU.
    pub fn guest_phys_addr_bits(&self) -> usize {
        crate::vcpu::pa_bits()
    }

    fn rollback_enable(&mut self, profile: IchCapabilityProfile) {
        disable_and_clear_ich(profile);
        VBAR_EL2.set(self.original_vbar_el2);
        HCR_EL2.set(self.original_hcr_el2);
        crate::host::clear_current_el_irq_handler();
        self.original_vbar_el2 = 0;
        self.original_hcr_el2 = 0;
    }
}

fn ensure_capability_can_be_published(
    cpu_id: usize,
    profile: IchCapabilityProfile,
) -> ArmVcpuResult {
    match crate::ich_capability(cpu_id) {
        Ok(published) if published == profile => Ok(()),
        Ok(published) => Err(crate::ArmVcpuError::IchCapabilityConflict {
            cpu_id,
            published,
            attempted: profile,
        }),
        Err(crate::ArmVcpuError::IchCapabilityNotPublished { .. }) => Ok(()),
        Err(error) => Err(error),
    }
}

fn disable_and_clear_ich(profile: IchCapabilityProfile) {
    ICH_HCR_EL2.set(0);
    for slot in 0..profile.list_register_count() {
        ich_lr_el2_set_raw(slot, 0);
    }
}
