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
use riscv::register::time;

use super::RISCVVCpu;
use crate::regs::GprIndex;

const KVM_REG_RISCV: u64 = 0x8000_0000_0000_0000;
const KVM_REG_SIZE_U64: u64 = 0x0030_0000_0000_0000;
const KVM_REG_RISCV_TYPE_MASK: u64 = 0x0000_0000_ff00_0000;
const KVM_REG_RISCV_SUBTYPE_MASK: u64 = 0x0000_0000_00ff_0000;
const KVM_REG_RISCV_CONFIG: u64 = 0x01 << 24;
const KVM_REG_RISCV_CORE: u64 = 0x02 << 24;
const KVM_REG_RISCV_CSR: u64 = 0x03 << 24;
const KVM_REG_RISCV_CSR_GENERAL: u64 = 0x00 << 16;
const KVM_REG_RISCV_TIMER: u64 = 0x04 << 24;
const KVM_REG_RISCV_ISA_EXT: u64 = 0x07 << 24;
const KVM_RISCV_BASE_ISA: u64 = (1 << 0) | (1 << 2) | (1 << 8) | (1 << 12);
const KVM_REG_RISCV_MODE_S: u64 = 1;
const KVM_RISCV_TIMER_STATE_OFF: u64 = 0;
const KVM_RISCV_TIMER_STATE_ON: u64 = 1;
const KVM_RISCV_TIMER_FREQUENCY: u64 = 10_000_000;
const KVM_RISCV_SATP_MODE_SV48: u64 = 9;
const KVM_REG_RISCV_CONFIG_ISA: u64 = 0;
const KVM_REG_RISCV_CONFIG_MVENDORID: u64 = 2;
const KVM_REG_RISCV_CONFIG_MARCHID: u64 = 3;
const KVM_REG_RISCV_CONFIG_MIMPID: u64 = 4;
const KVM_REG_RISCV_CONFIG_SATP_MODE: u64 = 6;
const KVM_REG_RISCV_CORE_PC: u64 = 0;
const KVM_REG_RISCV_CORE_MODE: u64 = 32;
const KVM_REG_RISCV_CSR_SSTATUS: u64 = 0;
const KVM_REG_RISCV_CSR_SIE: u64 = 1;
const KVM_REG_RISCV_CSR_STVEC: u64 = 2;
const KVM_REG_RISCV_CSR_SSCRATCH: u64 = 3;
const KVM_REG_RISCV_CSR_SEPC: u64 = 4;
const KVM_REG_RISCV_CSR_SCAUSE: u64 = 5;
const KVM_REG_RISCV_CSR_STVAL: u64 = 6;
const KVM_REG_RISCV_CSR_SIP: u64 = 7;
const KVM_REG_RISCV_CSR_SATP: u64 = 8;
const KVM_REG_RISCV_CSR_SCOUNTEREN: u64 = 9;
const KVM_REG_RISCV_CSR_SENVCFG: u64 = 10;
const KVM_REG_RISCV_TIMER_FREQUENCY_INDEX: u64 = 0;
const KVM_REG_RISCV_TIMER_TIME: u64 = 1;
const KVM_REG_RISCV_TIMER_COMPARE: u64 = 2;
const KVM_REG_RISCV_TIMER_STATE: u64 = 3;
const KVM_RISCV_ISA_EXT_A: u64 = 0;
const KVM_RISCV_ISA_EXT_C: u64 = 1;
const KVM_RISCV_ISA_EXT_D: u64 = 2;
const KVM_RISCV_ISA_EXT_F: u64 = 3;
const KVM_RISCV_ISA_EXT_I: u64 = 5;
const KVM_RISCV_ISA_EXT_M: u64 = 6;
const KVM_RISCV_ISA_EXT_ZICSR: u64 = 20;
const KVM_RISCV_ISA_EXT_ZIFENCEI: u64 = 21;

const RISCV_REG_IDS: [u64; 53] = [
    riscv_config_reg_id(KVM_REG_RISCV_CONFIG_ISA),
    riscv_config_reg_id(KVM_REG_RISCV_CONFIG_MVENDORID),
    riscv_config_reg_id(KVM_REG_RISCV_CONFIG_MARCHID),
    riscv_config_reg_id(KVM_REG_RISCV_CONFIG_MIMPID),
    riscv_config_reg_id(KVM_REG_RISCV_CONFIG_SATP_MODE),
    riscv_core_reg_id(0),
    riscv_core_reg_id(1),
    riscv_core_reg_id(2),
    riscv_core_reg_id(3),
    riscv_core_reg_id(4),
    riscv_core_reg_id(5),
    riscv_core_reg_id(6),
    riscv_core_reg_id(7),
    riscv_core_reg_id(8),
    riscv_core_reg_id(9),
    riscv_core_reg_id(10),
    riscv_core_reg_id(11),
    riscv_core_reg_id(12),
    riscv_core_reg_id(13),
    riscv_core_reg_id(14),
    riscv_core_reg_id(15),
    riscv_core_reg_id(16),
    riscv_core_reg_id(17),
    riscv_core_reg_id(18),
    riscv_core_reg_id(19),
    riscv_core_reg_id(20),
    riscv_core_reg_id(21),
    riscv_core_reg_id(22),
    riscv_core_reg_id(23),
    riscv_core_reg_id(24),
    riscv_core_reg_id(25),
    riscv_core_reg_id(26),
    riscv_core_reg_id(27),
    riscv_core_reg_id(28),
    riscv_core_reg_id(29),
    riscv_core_reg_id(30),
    riscv_core_reg_id(31),
    riscv_core_reg_id(32),
    riscv_csr_general_reg_id(KVM_REG_RISCV_CSR_SSTATUS),
    riscv_csr_general_reg_id(KVM_REG_RISCV_CSR_SIE),
    riscv_csr_general_reg_id(KVM_REG_RISCV_CSR_STVEC),
    riscv_csr_general_reg_id(KVM_REG_RISCV_CSR_SSCRATCH),
    riscv_csr_general_reg_id(KVM_REG_RISCV_CSR_SEPC),
    riscv_csr_general_reg_id(KVM_REG_RISCV_CSR_SCAUSE),
    riscv_csr_general_reg_id(KVM_REG_RISCV_CSR_STVAL),
    riscv_csr_general_reg_id(KVM_REG_RISCV_CSR_SIP),
    riscv_csr_general_reg_id(KVM_REG_RISCV_CSR_SATP),
    riscv_csr_general_reg_id(KVM_REG_RISCV_CSR_SCOUNTEREN),
    riscv_csr_general_reg_id(KVM_REG_RISCV_CSR_SENVCFG),
    riscv_timer_reg_id(KVM_REG_RISCV_TIMER_FREQUENCY_INDEX),
    riscv_timer_reg_id(KVM_REG_RISCV_TIMER_TIME),
    riscv_timer_reg_id(KVM_REG_RISCV_TIMER_COMPARE),
    riscv_timer_reg_id(KVM_REG_RISCV_TIMER_STATE),
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum KvmRiscvRegKind {
    Config(u64),
    Core(u64),
    CsrGeneral(u64),
    IsaExt(u64),
    Timer(u64),
}

impl RISCVVCpu {
    pub(super) fn get_kvm_arch_reg(&self, reg_id: u64) -> AxResult<u64> {
        match riscv_reg_kind(reg_id)? {
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
        match riscv_reg_kind(reg_id)? {
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

const fn riscv_config_reg_id(index: u64) -> u64 {
    KVM_REG_RISCV | KVM_REG_SIZE_U64 | KVM_REG_RISCV_CONFIG | index
}

const fn riscv_core_reg_id(index: u64) -> u64 {
    KVM_REG_RISCV | KVM_REG_SIZE_U64 | KVM_REG_RISCV_CORE | index
}

const fn riscv_csr_general_reg_id(index: u64) -> u64 {
    KVM_REG_RISCV | KVM_REG_SIZE_U64 | KVM_REG_RISCV_CSR | KVM_REG_RISCV_CSR_GENERAL | index
}

const fn riscv_timer_reg_id(index: u64) -> u64 {
    KVM_REG_RISCV | KVM_REG_SIZE_U64 | KVM_REG_RISCV_TIMER | index
}

const fn kvm_isa_ext_supported(index: u64) -> bool {
    matches!(
        index,
        KVM_RISCV_ISA_EXT_A
            | KVM_RISCV_ISA_EXT_C
            | KVM_RISCV_ISA_EXT_D
            | KVM_RISCV_ISA_EXT_F
            | KVM_RISCV_ISA_EXT_I
            | KVM_RISCV_ISA_EXT_M
            | KVM_RISCV_ISA_EXT_ZICSR
            | KVM_RISCV_ISA_EXT_ZIFENCEI
    )
}

fn riscv_reg_kind(reg_id: u64) -> AxResult<KvmRiscvRegKind> {
    if reg_id & (KVM_REG_RISCV | KVM_REG_SIZE_U64) != KVM_REG_RISCV | KVM_REG_SIZE_U64 {
        return Err(AxError::Unsupported);
    }

    let reg_type = reg_id & KVM_REG_RISCV_TYPE_MASK;
    let index = reg_id
        & !(KVM_REG_RISCV
            | KVM_REG_SIZE_U64
            | KVM_REG_RISCV_TYPE_MASK
            | KVM_REG_RISCV_SUBTYPE_MASK);
    match reg_type {
        KVM_REG_RISCV_CONFIG => Ok(KvmRiscvRegKind::Config(index)),
        KVM_REG_RISCV_CORE => Ok(KvmRiscvRegKind::Core(index)),
        KVM_REG_RISCV_CSR => match reg_id & KVM_REG_RISCV_SUBTYPE_MASK {
            KVM_REG_RISCV_CSR_GENERAL => Ok(KvmRiscvRegKind::CsrGeneral(index)),
            _ => Err(AxError::Unsupported),
        },
        KVM_REG_RISCV_TIMER => Ok(KvmRiscvRegKind::Timer(index)),
        KVM_REG_RISCV_ISA_EXT => Ok(KvmRiscvRegKind::IsaExt(index)),
        _ => Err(AxError::Unsupported),
    }
}
