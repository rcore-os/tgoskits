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

//! Hypervisor Interrupt Pending Register.

use bit_field::BitField;
use riscv::{clear, read_csr_as, set, set_clear_csr, write_csr};

/// Hypervisor Interrupt Registers.
#[derive(Copy, Clone, Debug)]
pub struct Hip {
    bits: usize,
}

impl Hip {
    /// Returns the raw bits of the register.
    #[inline]
    pub fn bits(&self) -> usize {
        self.bits
    }
    /// Creates a register value from raw bits.
    #[inline]
    pub fn from_bits(x: usize) -> Self {
        Hip { bits: x }
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
    /// Returns the virtual supervisor software interrupt pending.
    #[inline]
    pub fn vssip(&self) -> bool {
        self.bits.get_bit(2)
    }
    /// Sets the virtual supervisor software interrupt pending.
    #[inline]
    pub fn set_vssip(&mut self, val: bool) {
        self.bits.set_bit(2, val);
    }
    /// Returns the virtual supervisor timer interrupt pending.
    #[inline]
    pub fn vstip(&self) -> bool {
        self.bits.get_bit(6)
    }
    /// Sets the virtual supervisor timer interrupt pending.
    #[inline]
    pub fn set_vstip(&mut self, val: bool) {
        self.bits.set_bit(6, val);
    }
    /// Returns the virtual supervisor external interrupt pending.
    #[inline]
    pub fn vseip(&self) -> bool {
        self.bits.get_bit(10)
    }
    /// Sets the virtual supervisor external interrupt pending.
    #[inline]
    pub fn set_vseip(&mut self, val: bool) {
        self.bits.set_bit(10, val);
    }
    /// Returns the supervisor guest external interrupt pending.
    #[inline]
    pub fn sgeip(&self) -> bool {
        self.bits.get_bit(12)
    }
    /// Sets the supervisor guest external interrupt pending.
    #[inline]
    pub fn set_sgeip(&mut self, val: bool) {
        self.bits.set_bit(12, val);
    }
}

read_csr_as!(Hip, 0x644);
write_csr!(0x644);
set!(0x644);
clear!(0x644);

// bit ops
set_clear_csr!(
    /// Virtual supervisor software interrupt pending enable.
    , set_vssip, clear_vssip, 1 << 2);
set_clear_csr!(
    /// Virtual supervisor timer interrupt pending enable.
    , set_vstip, clear_vstip, 1 << 6);
set_clear_csr!(
    /// Virtual supervisor external interrupt pending enable.
    , set_vseip, clear_vseip, 1 << 10);
set_clear_csr!(
    /// Supervisor guest external interrupt pending enable.
    , set_sgeip, clear_sgeip, 1 << 12);

// enums
