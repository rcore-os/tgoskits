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

use riscv::register::sie;
use riscv_h::register::{hedeleg, hideleg, hvip};

use crate::{
    has_hardware_support,
    registers::{delegated_exception_bits, delegated_interrupt_bits},
    types::{RiscvVcpuError, RiscvVcpuResult},
};

/// Risc-V per-CPU state.
pub struct RiscvPerCpu {
    cpu_id: usize,
    enabled: bool,
}

impl RiscvPerCpu {
    /// Creates per-CPU virtualization state.
    pub fn new(cpu_id: usize) -> RiscvVcpuResult<Self> {
        Ok(Self {
            cpu_id,
            enabled: false,
        })
    }

    /// Whether virtualization has been enabled through this state object.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Enables RISC-V hypervisor state on this CPU.
    pub fn hardware_enable(&mut self) -> RiscvVcpuResult {
        if !has_hardware_support() {
            return Err(RiscvVcpuError::Unsupported);
        }
        unsafe {
            setup_csrs();
        }
        self.enabled = true;
        let _ = self.cpu_id;
        Ok(())
    }

    /// Disables guest-visible hypervisor state owned by this CPU state object.
    pub fn hardware_disable(&mut self) -> RiscvVcpuResult {
        unsafe {
            hvip::clear_vssip();
            hvip::clear_vstip();
            hvip::clear_vseip();
            core::arch::asm!("csrw hgatp, x0");
            core::arch::riscv64::hfence_gvma_all();
        }
        self.enabled = false;
        Ok(())
    }

    /// Returns the max guest page-table levels supported by this CPU.
    pub fn max_guest_page_table_levels(&self) -> usize {
        crate::max_guest_page_table_levels()
    }

    /// Returns the guest physical address width supported by this CPU.
    pub fn guest_phys_addr_bits(&self) -> usize {
        match crate::max_guest_page_table_levels() {
            3 => 41,
            4 => 50,
            _ => 0,
        }
    }
}

/// Backward-compatible per-CPU alias.
pub type RISCVPerCpu = RiscvPerCpu;

/// Initialize (H)S-level CSRs to a reasonable state.
unsafe fn setup_csrs() {
    unsafe {
        // Delegate some synchronous exceptions.
        hedeleg::Hedeleg::from_bits(delegated_exception_bits()).write();

        // Delegate all interrupts.
        hideleg::Hideleg::from_bits(delegated_interrupt_bits()).write();

        // Clear all interrupts.
        hvip::clear_vssip();
        hvip::clear_vstip();
        hvip::clear_vseip();

        // clear all interrupts.
        // the csr num of hcounteren is 0x606, the riscv repo is error!!!
        // hcounteren::Hcounteren::from_bits(0xffff_ffff).write();
        core::arch::asm!("csrw {csr}, {rs}", csr = const 0x606, rs = in(reg) -1);

        // enable interrupt
        sie::set_sext();
        sie::set_ssoft();
        sie::set_stimer();
    }
}
