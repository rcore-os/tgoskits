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

//! Virtual Supevisor Interrupt Enable Register.

use bit_field::BitField;
use riscv::{clear, read_csr_as, set, set_clear_csr, write_csr};

/// Virtual Supervisor Interrupt Enable Register.
#[derive(Copy, Clone, Debug)]
pub struct Vsie {
    bits: usize,
}

impl Vsie {
    /// Returns the raw bits of the register.
    #[inline]
    pub fn bits(&self) -> usize {
        self.bits
    }
    /// Creates a register value from raw bits.
    #[inline]
    pub fn from_bits(x: usize) -> Self {
        Vsie { bits: x }
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
    /// Returns the supervisor software interrupt enable.
    #[inline]
    pub fn ssie(&self) -> bool {
        self.bits.get_bit(1)
    }
    /// Sets the supervisor software interrupt enable.
    #[inline]
    pub fn set_ssie(&mut self, val: bool) {
        self.bits.set_bit(1, val);
    }
    /// Returns the supervisor timer interrupt enable.
    #[inline]
    pub fn stie(&self) -> bool {
        self.bits.get_bit(5)
    }
    /// Sets the supervisor timer interrupt enable.
    #[inline]
    pub fn set_stie(&mut self, val: bool) {
        self.bits.set_bit(5, val);
    }
    /// Returns the supervisor external interrupt enable.
    #[inline]
    pub fn seie(&self) -> bool {
        self.bits.get_bit(9)
    }
    /// Sets the supervisor external interrupt enable.
    #[inline]
    pub fn set_seie(&mut self, val: bool) {
        self.bits.set_bit(9, val);
    }
}

read_csr_as!(Vsie, 0x204);
write_csr!(0x204);
set!(0x204);
clear!(0x204);
// bit ops
set_clear_csr!(
    /// Supervisor software interrupt enable.
    , set_ssie, clear_ssie, 1 << 1);
set_clear_csr!(
    /// Supervisor timer interrupt enable.
    , set_stie, clear_stie, 1 << 5);
set_clear_csr!(
    /// Supervisor external interrupt enable.
    , set_seie, clear_seie, 1 << 9);

// enums
