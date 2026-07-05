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

//! KVM one-reg identifiers for RISC-V vCPU state.
//!
//! This module knows how to construct and classify register IDs. Reading or
//! writing the actual vCPU state remains the responsibility of the vCPU crate.

use crate::{KvmUapiError, Result};

pub const KVM_REG_RISCV: u64 = 0x8000_0000_0000_0000;
pub const KVM_REG_SIZE_U64: u64 = 0x0030_0000_0000_0000;
pub const KVM_REG_RISCV_TYPE_MASK: u64 = 0x0000_0000_ff00_0000;
pub const KVM_REG_RISCV_SUBTYPE_MASK: u64 = 0x0000_0000_00ff_0000;
pub const KVM_REG_RISCV_CONFIG: u64 = 0x01 << 24;
pub const KVM_REG_RISCV_CORE: u64 = 0x02 << 24;
pub const KVM_REG_RISCV_CSR: u64 = 0x03 << 24;
pub const KVM_REG_RISCV_CSR_GENERAL: u64 = 0x00 << 16;
pub const KVM_REG_RISCV_TIMER: u64 = 0x04 << 24;
pub const KVM_REG_RISCV_ISA_EXT: u64 = 0x07 << 24;
pub const KVM_RISCV_BASE_ISA: u64 = (1 << 0) | (1 << 2) | (1 << 8) | (1 << 12);
pub const KVM_REG_RISCV_MODE_S: u64 = 1;
pub const KVM_RISCV_TIMER_STATE_OFF: u64 = 0;
pub const KVM_RISCV_TIMER_STATE_ON: u64 = 1;
pub const KVM_RISCV_TIMER_FREQUENCY: u64 = 10_000_000;
pub const KVM_RISCV_SATP_MODE_SV48: u64 = 9;
pub const KVM_REG_RISCV_CONFIG_ISA: u64 = 0;
pub const KVM_REG_RISCV_CONFIG_MVENDORID: u64 = 2;
pub const KVM_REG_RISCV_CONFIG_MARCHID: u64 = 3;
pub const KVM_REG_RISCV_CONFIG_MIMPID: u64 = 4;
pub const KVM_REG_RISCV_CONFIG_SATP_MODE: u64 = 6;
pub const KVM_REG_RISCV_CORE_PC: u64 = 0;
pub const KVM_REG_RISCV_CORE_MODE: u64 = 32;
pub const KVM_REG_RISCV_CSR_SSTATUS: u64 = 0;
pub const KVM_REG_RISCV_CSR_SIE: u64 = 1;
pub const KVM_REG_RISCV_CSR_STVEC: u64 = 2;
pub const KVM_REG_RISCV_CSR_SSCRATCH: u64 = 3;
pub const KVM_REG_RISCV_CSR_SEPC: u64 = 4;
pub const KVM_REG_RISCV_CSR_SCAUSE: u64 = 5;
pub const KVM_REG_RISCV_CSR_STVAL: u64 = 6;
pub const KVM_REG_RISCV_CSR_SIP: u64 = 7;
pub const KVM_REG_RISCV_CSR_SATP: u64 = 8;
pub const KVM_REG_RISCV_CSR_SCOUNTEREN: u64 = 9;
pub const KVM_REG_RISCV_CSR_SENVCFG: u64 = 10;
pub const KVM_REG_RISCV_TIMER_FREQUENCY_INDEX: u64 = 0;
pub const KVM_REG_RISCV_TIMER_TIME: u64 = 1;
pub const KVM_REG_RISCV_TIMER_COMPARE: u64 = 2;
pub const KVM_REG_RISCV_TIMER_STATE: u64 = 3;
pub const KVM_RISCV_ISA_EXT_A: u64 = 0;
pub const KVM_RISCV_ISA_EXT_C: u64 = 1;
pub const KVM_RISCV_ISA_EXT_D: u64 = 2;
pub const KVM_RISCV_ISA_EXT_F: u64 = 3;
pub const KVM_RISCV_ISA_EXT_I: u64 = 5;
pub const KVM_RISCV_ISA_EXT_M: u64 = 6;
pub const KVM_RISCV_ISA_EXT_ZICSR: u64 = 20;
pub const KVM_RISCV_ISA_EXT_ZIFENCEI: u64 = 21;

/// Register IDs reported by `KVM_GET_REG_LIST` for the supported RISC-V subset.
pub const RISCV_REG_IDS: [u64; 53] = [
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
pub enum KvmRiscvRegKind {
    Config(u64),
    Core(u64),
    CsrGeneral(u64),
    IsaExt(u64),
    Timer(u64),
}

pub const fn riscv_config_reg_id(index: u64) -> u64 {
    KVM_REG_RISCV | KVM_REG_SIZE_U64 | KVM_REG_RISCV_CONFIG | index
}

pub const fn riscv_core_reg_id(index: u64) -> u64 {
    KVM_REG_RISCV | KVM_REG_SIZE_U64 | KVM_REG_RISCV_CORE | index
}

pub const fn riscv_csr_general_reg_id(index: u64) -> u64 {
    KVM_REG_RISCV | KVM_REG_SIZE_U64 | KVM_REG_RISCV_CSR | KVM_REG_RISCV_CSR_GENERAL | index
}

pub const fn riscv_timer_reg_id(index: u64) -> u64 {
    KVM_REG_RISCV | KVM_REG_SIZE_U64 | KVM_REG_RISCV_TIMER | index
}

pub const fn kvm_isa_ext_supported(index: u64) -> bool {
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

/// Classifies a RISC-V KVM one-reg ID into the state group it addresses.
pub fn riscv_reg_kind(reg_id: u64) -> Result<KvmRiscvRegKind> {
    if reg_id & (KVM_REG_RISCV | KVM_REG_SIZE_U64) != KVM_REG_RISCV | KVM_REG_SIZE_U64 {
        return Err(KvmUapiError::Unsupported);
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
            _ => Err(KvmUapiError::Unsupported),
        },
        KVM_REG_RISCV_TIMER => Ok(KvmRiscvRegKind::Timer(index)),
        KVM_REG_RISCV_ISA_EXT => Ok(KvmRiscvRegKind::IsaExt(index)),
        _ => Err(KvmUapiError::Unsupported),
    }
}
