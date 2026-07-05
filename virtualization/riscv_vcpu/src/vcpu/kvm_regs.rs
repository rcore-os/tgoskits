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

use ax_errno::{AxError, AxResult};
use kvm_uapi::riscv::*;
use riscv::register::time;

use super::RISCVVCpu;
use crate::regs::GprIndex;

// kvm-uapi classifies RISC-V one-reg IDs; this file maps those IDs to the
// concrete RISCVVCpu register storage.

impl RISCVVCpu {
    pub(super) fn get_kvm_arch_reg(&self, reg_id: u64) -> AxResult<u64> {
        match riscv_reg_kind(reg_id).map_err(map_kvm_uapi_error)? {
            KvmRiscvRegKind::Config(index) => self.get_kvm_config_reg(index),
            KvmRiscvRegKind::Core(index) => self.get_kvm_core_reg(index),
            KvmRiscvRegKind::CsrGeneral(index) => self.get_kvm_csr_general_reg(index),
            KvmRiscvRegKind::IsaExt(index) => self.get_kvm_isa_ext_reg(index),
            KvmRiscvRegKind::Timer(index) => self.get_kvm_timer_reg(index),
        }
    }

    pub(super) fn kvm_arch_reg_ids(&self) -> &'static [u64] {
        &RISCV_REG_IDS
    }

    pub(super) fn set_kvm_arch_reg(&mut self, reg_id: u64, value: u64) -> AxResult {
        match riscv_reg_kind(reg_id).map_err(map_kvm_uapi_error)? {
            KvmRiscvRegKind::Config(index) => self.set_kvm_config_reg(index, value),
            KvmRiscvRegKind::Core(index) => self.set_kvm_core_reg(index, value),
            KvmRiscvRegKind::CsrGeneral(index) => self.set_kvm_csr_general_reg(index, value),
            KvmRiscvRegKind::IsaExt(index) => self.set_kvm_isa_ext_reg(index, value),
            KvmRiscvRegKind::Timer(index) => self.set_kvm_timer_reg(index, value),
        }
    }

    fn get_kvm_config_reg(&self, index: u64) -> AxResult<u64> {
        match index {
            KVM_REG_RISCV_CONFIG_ISA => Ok(KVM_RISCV_BASE_ISA),
            KVM_REG_RISCV_CONFIG_MVENDORID
            | KVM_REG_RISCV_CONFIG_MARCHID
            | KVM_REG_RISCV_CONFIG_MIMPID => Ok(0),
            KVM_REG_RISCV_CONFIG_SATP_MODE => Ok(KVM_RISCV_SATP_MODE_SV48),
            _ => Err(AxError::Unsupported),
        }
    }

    fn set_kvm_config_reg(&mut self, index: u64, value: u64) -> AxResult {
        if self.get_kvm_config_reg(index)? == value {
            Ok(())
        } else {
            Err(AxError::InvalidInput)
        }
    }

    fn get_kvm_core_reg(&self, index: u64) -> AxResult<u64> {
        match index {
            KVM_REG_RISCV_CORE_PC => Ok(self.regs.guest_regs.sepc as u64),
            1..=31 => {
                let Some(gpr) = GprIndex::from_raw(index as u32) else {
                    return Err(AxError::InvalidInput);
                };
                Ok(self.regs.guest_regs.gprs.reg(gpr) as u64)
            }
            KVM_REG_RISCV_CORE_MODE => Ok(KVM_REG_RISCV_MODE_S),
            _ => Err(AxError::Unsupported),
        }
    }

    fn set_kvm_core_reg(&mut self, index: u64, value: u64) -> AxResult {
        match index {
            KVM_REG_RISCV_CORE_PC => {
                self.regs.guest_regs.sepc = value as usize;
                Ok(())
            }
            1..=31 => {
                let Some(gpr) = GprIndex::from_raw(index as u32) else {
                    return Err(AxError::InvalidInput);
                };
                self.regs.guest_regs.gprs.set_reg(gpr, value as usize);
                Ok(())
            }
            KVM_REG_RISCV_CORE_MODE if value == KVM_REG_RISCV_MODE_S => Ok(()),
            KVM_REG_RISCV_CORE_MODE => Err(AxError::Unsupported),
            _ => Err(AxError::Unsupported),
        }
    }

    fn get_kvm_csr_general_reg(&self, index: u64) -> AxResult<u64> {
        match index {
            KVM_REG_RISCV_CSR_SSTATUS => Ok(self.regs.vs_csrs.vsstatus as u64),
            KVM_REG_RISCV_CSR_SIE => Ok(self.regs.vs_csrs.vsie as u64),
            KVM_REG_RISCV_CSR_STVEC => Ok(self.regs.vs_csrs.vstvec as u64),
            KVM_REG_RISCV_CSR_SSCRATCH => Ok(self.regs.vs_csrs.vsscratch as u64),
            KVM_REG_RISCV_CSR_SEPC => Ok(self.regs.vs_csrs.vsepc as u64),
            KVM_REG_RISCV_CSR_SCAUSE => Ok(self.regs.vs_csrs.vscause as u64),
            KVM_REG_RISCV_CSR_STVAL => Ok(self.regs.vs_csrs.vstval as u64),
            KVM_REG_RISCV_CSR_SIP => Ok(self.regs.virtual_hs_csrs.hvip as u64),
            KVM_REG_RISCV_CSR_SATP => Ok(self.regs.vs_csrs.vsatp as u64),
            KVM_REG_RISCV_CSR_SCOUNTEREN => Ok(self.regs.guest_regs.scounteren as u64),
            KVM_REG_RISCV_CSR_SENVCFG => Ok(0),
            _ => Err(AxError::Unsupported),
        }
    }

    fn set_kvm_csr_general_reg(&mut self, index: u64, value: u64) -> AxResult {
        match index {
            KVM_REG_RISCV_CSR_SSTATUS => self.regs.vs_csrs.vsstatus = value as usize,
            KVM_REG_RISCV_CSR_SIE => self.regs.vs_csrs.vsie = value as usize,
            KVM_REG_RISCV_CSR_STVEC => self.regs.vs_csrs.vstvec = value as usize,
            KVM_REG_RISCV_CSR_SSCRATCH => self.regs.vs_csrs.vsscratch = value as usize,
            KVM_REG_RISCV_CSR_SEPC => self.regs.vs_csrs.vsepc = value as usize,
            KVM_REG_RISCV_CSR_SCAUSE => self.regs.vs_csrs.vscause = value as usize,
            KVM_REG_RISCV_CSR_STVAL => self.regs.vs_csrs.vstval = value as usize,
            KVM_REG_RISCV_CSR_SIP => self.regs.virtual_hs_csrs.hvip = value as usize,
            KVM_REG_RISCV_CSR_SATP => self.regs.vs_csrs.vsatp = value as usize,
            KVM_REG_RISCV_CSR_SCOUNTEREN => self.regs.guest_regs.scounteren = value as usize,
            KVM_REG_RISCV_CSR_SENVCFG if value == 0 => {}
            KVM_REG_RISCV_CSR_SENVCFG => return Err(AxError::InvalidInput),
            _ => return Err(AxError::Unsupported),
        }
        Ok(())
    }

    fn get_kvm_isa_ext_reg(&self, index: u64) -> AxResult<u64> {
        Ok(u64::from(kvm_isa_ext_supported(index)))
    }

    fn set_kvm_isa_ext_reg(&mut self, index: u64, value: u64) -> AxResult {
        match (kvm_isa_ext_supported(index), value) {
            (true, 0 | 1) => Ok(()),
            (true, _) => Err(AxError::InvalidInput),
            (false, 0) => Ok(()),
            (false, _) => Err(AxError::Unsupported),
        }
    }

    fn get_kvm_timer_reg(&self, index: u64) -> AxResult<u64> {
        match index {
            KVM_REG_RISCV_TIMER_FREQUENCY_INDEX => Ok(KVM_RISCV_TIMER_FREQUENCY),
            KVM_REG_RISCV_TIMER_TIME => Ok(time::read64()),
            KVM_REG_RISCV_TIMER_COMPARE => Ok(self.regs.vs_csrs.vstimecmp as u64),
            KVM_REG_RISCV_TIMER_STATE => Ok(if self.regs.vs_csrs.vstimecmp == usize::MAX {
                KVM_RISCV_TIMER_STATE_OFF
            } else {
                KVM_RISCV_TIMER_STATE_ON
            }),
            _ => Err(AxError::Unsupported),
        }
    }

    fn set_kvm_timer_reg(&mut self, index: u64, value: u64) -> AxResult {
        match index {
            KVM_REG_RISCV_TIMER_FREQUENCY_INDEX if value == KVM_RISCV_TIMER_FREQUENCY => Ok(()),
            KVM_REG_RISCV_TIMER_FREQUENCY_INDEX => Err(AxError::InvalidInput),
            KVM_REG_RISCV_TIMER_TIME => Ok(()),
            KVM_REG_RISCV_TIMER_COMPARE => {
                self.regs.vs_csrs.vstimecmp = value as usize;
                Ok(())
            }
            KVM_REG_RISCV_TIMER_STATE if value == KVM_RISCV_TIMER_STATE_ON => Ok(()),
            KVM_REG_RISCV_TIMER_STATE if value == KVM_RISCV_TIMER_STATE_OFF => {
                self.regs.vs_csrs.vstimecmp = usize::MAX;
                Ok(())
            }
            KVM_REG_RISCV_TIMER_STATE => Err(AxError::InvalidInput),
            _ => Err(AxError::Unsupported),
        }
    }
}

fn map_kvm_uapi_error(_err: kvm_uapi::KvmUapiError) -> AxError {
    AxError::Unsupported
}
