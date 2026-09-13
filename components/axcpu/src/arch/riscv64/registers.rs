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

//! Architecture register images.

pub use riscv::register::sstatus::{self, FS, Sstatus};

#[cfg(kernel_tls)]
pub use super::asm::{read_thread_pointer, write_thread_pointer};
pub use super::{
    context::FpState,
    local_state::{CpuEntryState, TaskEntryState},
};

/// General registers of RISC-V.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct GeneralRegisters {
    /// Architectural x0 (zero) register slot.
    pub zero: usize,
    /// Architectural x1 (ra) register slot.
    pub ra: usize,
    /// Architectural x2 (sp) register slot.
    pub sp: usize,
    /// Architectural x3 (gp) register slot.
    pub gp: usize,
    /// Architectural x4 (tp) register slot.
    pub tp: usize,
    /// Architectural x5 (t0) register slot.
    pub t0: usize,
    /// Architectural x6 (t1) register slot.
    pub t1: usize,
    /// Architectural x7 (t2) register slot.
    pub t2: usize,
    /// Architectural x8 (s0) register slot.
    pub s0: usize,
    /// Architectural x9 (s1) register slot.
    pub s1: usize,
    /// Architectural x10 (a0) register slot.
    pub a0: usize,
    /// Architectural x11 (a1) register slot.
    pub a1: usize,
    /// Architectural x12 (a2) register slot.
    pub a2: usize,
    /// Architectural x13 (a3) register slot.
    pub a3: usize,
    /// Architectural x14 (a4) register slot.
    pub a4: usize,
    /// Architectural x15 (a5) register slot.
    pub a5: usize,
    /// Architectural x16 (a6) register slot.
    pub a6: usize,
    /// Architectural x17 (a7) register slot.
    pub a7: usize,
    /// Architectural x18 (s2) register slot.
    pub s2: usize,
    /// Architectural x19 (s3) register slot.
    pub s3: usize,
    /// Architectural x20 (s4) register slot.
    pub s4: usize,
    /// Architectural x21 (s5) register slot.
    pub s5: usize,
    /// Architectural x22 (s6) register slot.
    pub s6: usize,
    /// Architectural x23 (s7) register slot.
    pub s7: usize,
    /// Architectural x24 (s8) register slot.
    pub s8: usize,
    /// Architectural x25 (s9) register slot.
    pub s9: usize,
    /// Architectural x26 (s10) register slot.
    pub s10: usize,
    /// Architectural x27 (s11) register slot.
    pub s11: usize,
    /// Architectural x28 (t3) register slot.
    pub t3: usize,
    /// Architectural x29 (t4) register slot.
    pub t4: usize,
    /// Architectural x30 (t5) register slot.
    pub t5: usize,
    /// Architectural x31 (t6) register slot.
    pub t6: usize,
}

/// Architectural integer register number, including the hardwired zero register.
#[repr(u32)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GprIndex {
    /// x0 (zero).
    Zero = 0,
    /// x1 (ra).
    RA   = 1,
    /// x2 (sp).
    SP   = 2,
    /// x3 (gp).
    GP   = 3,
    /// x4 (tp).
    TP   = 4,
    /// x5 (t0).
    T0   = 5,
    /// x6 (t1).
    T1   = 6,
    /// x7 (t2).
    T2   = 7,
    /// x8 (s0).
    S0   = 8,
    /// x9 (s1).
    S1   = 9,
    /// x10 (a0).
    A0   = 10,
    /// x11 (a1).
    A1   = 11,
    /// x12 (a2).
    A2   = 12,
    /// x13 (a3).
    A3   = 13,
    /// x14 (a4).
    A4   = 14,
    /// x15 (a5).
    A5   = 15,
    /// x16 (a6).
    A6   = 16,
    /// x17 (a7).
    A7   = 17,
    /// x18 (s2).
    S2   = 18,
    /// x19 (s3).
    S3   = 19,
    /// x20 (s4).
    S4   = 20,
    /// x21 (s5).
    S5   = 21,
    /// x22 (s6).
    S6   = 22,
    /// x23 (s7).
    S7   = 23,
    /// x24 (s8).
    S8   = 24,
    /// x25 (s9).
    S9   = 25,
    /// x26 (s10).
    S10  = 26,
    /// x27 (s11).
    S11  = 27,
    /// x28 (t3).
    T3   = 28,
    /// x29 (t4).
    T4   = 29,
    /// x30 (t5).
    T5   = 30,
    /// x31 (t6).
    T6   = 31,
}

impl GprIndex {
    /// Checks a raw architectural register number.
    pub const fn from_raw(raw: u32) -> Option<Self> {
        match raw {
            0 => Some(Self::Zero),
            1 => Some(Self::RA),
            2 => Some(Self::SP),
            3 => Some(Self::GP),
            4 => Some(Self::TP),
            5 => Some(Self::T0),
            6 => Some(Self::T1),
            7 => Some(Self::T2),
            8 => Some(Self::S0),
            9 => Some(Self::S1),
            10 => Some(Self::A0),
            11 => Some(Self::A1),
            12 => Some(Self::A2),
            13 => Some(Self::A3),
            14 => Some(Self::A4),
            15 => Some(Self::A5),
            16 => Some(Self::A6),
            17 => Some(Self::A7),
            18 => Some(Self::S2),
            19 => Some(Self::S3),
            20 => Some(Self::S4),
            21 => Some(Self::S5),
            22 => Some(Self::S6),
            23 => Some(Self::S7),
            24 => Some(Self::S8),
            25 => Some(Self::S9),
            26 => Some(Self::S10),
            27 => Some(Self::S11),
            28 => Some(Self::T3),
            29 => Some(Self::T4),
            30 => Some(Self::T5),
            31 => Some(Self::T6),
            _ => None,
        }
    }

    /// Returns the register slot offset in the shared saved GPR image.
    pub const fn byte_offset(self) -> usize {
        match self {
            Self::Zero => core::mem::offset_of!(GeneralRegisters, zero),
            Self::RA => core::mem::offset_of!(GeneralRegisters, ra),
            Self::SP => core::mem::offset_of!(GeneralRegisters, sp),
            Self::GP => core::mem::offset_of!(GeneralRegisters, gp),
            Self::TP => core::mem::offset_of!(GeneralRegisters, tp),
            Self::T0 => core::mem::offset_of!(GeneralRegisters, t0),
            Self::T1 => core::mem::offset_of!(GeneralRegisters, t1),
            Self::T2 => core::mem::offset_of!(GeneralRegisters, t2),
            Self::S0 => core::mem::offset_of!(GeneralRegisters, s0),
            Self::S1 => core::mem::offset_of!(GeneralRegisters, s1),
            Self::A0 => core::mem::offset_of!(GeneralRegisters, a0),
            Self::A1 => core::mem::offset_of!(GeneralRegisters, a1),
            Self::A2 => core::mem::offset_of!(GeneralRegisters, a2),
            Self::A3 => core::mem::offset_of!(GeneralRegisters, a3),
            Self::A4 => core::mem::offset_of!(GeneralRegisters, a4),
            Self::A5 => core::mem::offset_of!(GeneralRegisters, a5),
            Self::A6 => core::mem::offset_of!(GeneralRegisters, a6),
            Self::A7 => core::mem::offset_of!(GeneralRegisters, a7),
            Self::S2 => core::mem::offset_of!(GeneralRegisters, s2),
            Self::S3 => core::mem::offset_of!(GeneralRegisters, s3),
            Self::S4 => core::mem::offset_of!(GeneralRegisters, s4),
            Self::S5 => core::mem::offset_of!(GeneralRegisters, s5),
            Self::S6 => core::mem::offset_of!(GeneralRegisters, s6),
            Self::S7 => core::mem::offset_of!(GeneralRegisters, s7),
            Self::S8 => core::mem::offset_of!(GeneralRegisters, s8),
            Self::S9 => core::mem::offset_of!(GeneralRegisters, s9),
            Self::S10 => core::mem::offset_of!(GeneralRegisters, s10),
            Self::S11 => core::mem::offset_of!(GeneralRegisters, s11),
            Self::T3 => core::mem::offset_of!(GeneralRegisters, t3),
            Self::T4 => core::mem::offset_of!(GeneralRegisters, t4),
            Self::T5 => core::mem::offset_of!(GeneralRegisters, t5),
            Self::T6 => core::mem::offset_of!(GeneralRegisters, t6),
        }
    }
}

impl GeneralRegisters {
    /// Reads a register from this image. Architectural x0 always reads zero.
    pub const fn reg(&self, index: GprIndex) -> usize {
        match index {
            GprIndex::Zero => 0,
            GprIndex::RA => self.ra,
            GprIndex::SP => self.sp,
            GprIndex::GP => self.gp,
            GprIndex::TP => self.tp,
            GprIndex::T0 => self.t0,
            GprIndex::T1 => self.t1,
            GprIndex::T2 => self.t2,
            GprIndex::S0 => self.s0,
            GprIndex::S1 => self.s1,
            GprIndex::A0 => self.a0,
            GprIndex::A1 => self.a1,
            GprIndex::A2 => self.a2,
            GprIndex::A3 => self.a3,
            GprIndex::A4 => self.a4,
            GprIndex::A5 => self.a5,
            GprIndex::A6 => self.a6,
            GprIndex::A7 => self.a7,
            GprIndex::S2 => self.s2,
            GprIndex::S3 => self.s3,
            GprIndex::S4 => self.s4,
            GprIndex::S5 => self.s5,
            GprIndex::S6 => self.s6,
            GprIndex::S7 => self.s7,
            GprIndex::S8 => self.s8,
            GprIndex::S9 => self.s9,
            GprIndex::S10 => self.s10,
            GprIndex::S11 => self.s11,
            GprIndex::T3 => self.t3,
            GprIndex::T4 => self.t4,
            GprIndex::T5 => self.t5,
            GprIndex::T6 => self.t6,
        }
    }

    /// Writes a saved register; writes to architectural x0 are ignored.
    pub fn set_reg(&mut self, index: GprIndex, value: usize) {
        match index {
            GprIndex::Zero => {}
            GprIndex::RA => self.ra = value,
            GprIndex::SP => self.sp = value,
            GprIndex::GP => self.gp = value,
            GprIndex::TP => self.tp = value,
            GprIndex::T0 => self.t0 = value,
            GprIndex::T1 => self.t1 = value,
            GprIndex::T2 => self.t2 = value,
            GprIndex::S0 => self.s0 = value,
            GprIndex::S1 => self.s1 = value,
            GprIndex::A0 => self.a0 = value,
            GprIndex::A1 => self.a1 = value,
            GprIndex::A2 => self.a2 = value,
            GprIndex::A3 => self.a3 = value,
            GprIndex::A4 => self.a4 = value,
            GprIndex::A5 => self.a5 = value,
            GprIndex::A6 => self.a6 = value,
            GprIndex::A7 => self.a7 = value,
            GprIndex::S2 => self.s2 = value,
            GprIndex::S3 => self.s3 = value,
            GprIndex::S4 => self.s4 = value,
            GprIndex::S5 => self.s5 = value,
            GprIndex::S6 => self.s6 = value,
            GprIndex::S7 => self.s7 = value,
            GprIndex::S8 => self.s8 = value,
            GprIndex::S9 => self.s9 = value,
            GprIndex::S10 => self.s10 = value,
            GprIndex::S11 => self.s11 = value,
            GprIndex::T3 => self.t3 = value,
            GprIndex::T4 => self.t4 = value,
            GprIndex::T5 => self.t5 = value,
            GprIndex::T6 => self.t6 = value,
        }
    }

    /// Copies a0 through a7 without borrowing the saved register image.
    pub const fn a_regs(&self) -> [usize; 8] {
        [
            self.a0, self.a1, self.a2, self.a3, self.a4, self.a5, self.a6, self.a7,
        ]
    }
}

/// Reads the raw `tp` register; its software meaning belongs to the caller.
#[inline]
pub fn read_tp() -> usize {
    let value;
    // SAFETY: this only copies the current register; no pointer is followed.
    unsafe { core::arch::asm!("mv {}, tp", out(reg) value, options(nostack)) };
    value
}

/// Installs the raw `tp` register.
///
/// # Safety
/// The caller must own the final TLS or task-anchor transition. The selected
/// object must remain alive and compatible with every following instruction
/// and exception entry; no Rust code may observe an intermediate binding.
#[inline]
pub unsafe fn write_tp(value: usize) {
    // SAFETY: the caller owns the register transition and its referenced state.
    unsafe { core::arch::asm!("mv tp, {}", in(reg) value, options(nostack)) };
}

/// Reads supervisor scratch without interpreting its software-owned contents.
#[inline]
pub fn read_sscratch() -> usize {
    let value;
    // SAFETY: privileged code may read this CSR; no pointer is followed.
    unsafe { core::arch::asm!("csrr {}, sscratch", out(reg) value, options(nostack)) };
    value
}

/// Installs supervisor scratch.
///
/// # Safety
/// The caller must exclude supervisor trap entry until the new scratch value
/// agrees with the installed entry convention. Any referenced memory must
/// remain alive for the complete interval in which the register is installed.
#[inline]
pub unsafe fn write_sscratch(value: usize) {
    // SAFETY: the caller excludes entry and owns the new scratch binding.
    unsafe { core::arch::asm!("csrw sscratch, {}", in(reg) value, options(nostack)) };
}
