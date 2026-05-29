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

//! Virtual Supervisor Trap Vector Base Address Register.

use bit_field::BitField;
use riscv::{clear, read_csr_as, set, write_csr};

/// Virtual Supervisor Trap Vector Base Address Register.
#[derive(Copy, Clone, Debug)]
pub struct Vstvec {
    bits: usize,
}

impl Vstvec {
    /// Returns the raw bits of the register.
    #[inline]
    pub fn bits(&self) -> usize {
        self.bits
    }
    /// Creates a register value from raw bits.
    #[inline]
    pub fn from_bits(x: usize) -> Self {
        Vstvec { bits: x }
    }
    /// Writes the register value to the CSR.
    ///
    /// # Safety
    ///
    /// This function is unsafe because writing to CSR registers can have
    /// system-wide effects and may violate memory safety guarantees.
    #[inline]
    pub unsafe fn write(&self) {
        // SAFETY: Caller ensures this is safe to execute
        unsafe { _write(self.bits) };
    }
    /// Returns the base address of the virtual supervisor trap vector.
    #[inline]
    pub fn base(&self) -> usize {
        self.bits.get_bits(2..64)
    }
    /// Sets the base address of the virtual supervisor trap vector.
    #[inline]
    pub fn set_base(&mut self, val: usize) {
        self.bits.set_bits(2..64, val);
    }
    /// Returns the mode of the virtual supervisor trap vector.
    #[inline]
    pub fn mode(&self) -> usize {
        self.bits.get_bits(0..2)
    }
    /// Sets the mode of the virtual supervisor trap vector.
    #[inline]
    pub fn set_mode(&mut self, val: usize) {
        self.bits.set_bits(0..2, val);
    }
}

read_csr_as!(Vstvec, 0x205);
write_csr!(0x205);
set!(0x205);
clear!(0x205);
// bit ops

// enums
