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

use ax_cpu::virtualization::{PerCpu, VirtualizationError};

use crate::arch::aarch64::policy::{ArmVcpuError, ArmVcpuResult};

/// Host integration for the current CPU's EL2 ownership.
pub struct ArmPerCpu {
    machine: PerCpu,
    timer_frequency_hz: u64,
}

impl ArmPerCpu {
    /// Records the platform counter frequency without enabling virtualization.
    pub fn new(_cpu_id: usize) -> ArmVcpuResult<Self> {
        let timer_frequency_hz = ax_cpu::timer::counter_frequency();
        if timer_frequency_hz == 0 {
            return Err(ArmVcpuError::Unsupported);
        }
        Ok(Self {
            machine: PerCpu::new(),
            timer_frequency_hz,
        })
    }
    /// Reports this owner's active EL2 installation.
    pub fn is_enabled(&self) -> bool {
        self.machine.is_enabled()
    }
    /// Installs host dispatch before enabling the guest-capable vector.
    pub fn hardware_enable(&mut self) -> ArmVcpuResult {
        if self.machine.is_enabled() {
            return Err(ArmVcpuError::BadState);
        }
        // SAFETY: AxVM holds its current-CPU IRQ/preemption guard and retains
        // the statically linked vector until every guest and owner is retired.
        let result = unsafe { self.machine.enable(ax_cpu::virtualization::guest_vector()) };
        result.map_err(cpu_error)
    }
    /// Restores host hardware before withdrawing IRQ dispatch.
    pub fn hardware_disable(&mut self) -> ArmVcpuResult {
        // SAFETY: AxVM unloads guests on this CPU under its IRQ/preemption guard.
        unsafe { self.machine.disable() }.map_err(cpu_error)?;
        Ok(())
    }
    /// Returns the implemented guest table depth selected by the host policy.
    pub fn max_guest_page_table_levels(&self) -> usize {
        super::vcpu::max_gpt_level(super::vcpu::pa_bits())
    }
    /// Returns the current CPU's physical address width.
    pub fn guest_phys_addr_bits(&self) -> usize {
        super::vcpu::pa_bits()
    }
    /// Returns this CPU's recorded architectural counter frequency.
    pub const fn timer_frequency_hz(&self) -> u64 {
        self.timer_frequency_hz
    }
}

fn cpu_error(error: VirtualizationError) -> ArmVcpuError {
    match error {
        VirtualizationError::AlreadyEnabled | VirtualizationError::NotEnabled => {
            ArmVcpuError::BadState
        }
        VirtualizationError::InvalidVector | VirtualizationError::InvalidRoot => {
            ArmVcpuError::InvalidInput
        }
        VirtualizationError::Unavailable
        | VirtualizationError::UnsupportedPaging
        | VirtualizationError::UnsupportedTimer => ArmVcpuError::Unsupported,
    }
}
