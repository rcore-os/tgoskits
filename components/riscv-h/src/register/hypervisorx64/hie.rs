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

//! Hypervisor Interrupt Enable Register.
//!
//! The `hie` register controls which virtual supervisor interrupts can be taken
//! when executing in VS-mode or VU-mode. It contains enable bits for:
//! - Virtual supervisor software interrupts (VSSIE)
//! - Virtual supervisor timer interrupts (VSTIE)  
//! - Virtual supervisor external interrupts (VSEIE)
//!
//! This register works in conjunction with the `hvip` register (interrupt pending)
//! and guest interrupt delegation to manage virtualized interrupt delivery.

use bit_field::BitField;
use riscv::{clear, read_csr_as, set, set_clear_csr, write_csr};

/// Hypervisor Interrupt Enable Register.
#[derive(Copy, Clone, Debug)]
pub struct Hie {
    bits: usize,
}

impl Hie {
    /// Returns the raw bits of the register.
    #[inline]
    pub fn bits(&self) -> usize {
        self.bits
    }
    /// Creates a register value from raw bits.
    #[inline]
    pub fn from_bits(x: usize) -> Self {
        Hie { bits: x }
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
    /// Returns the status of the virtual supervisor software interrupt enable.
    #[inline]
    pub fn vssie(&self) -> bool {
        self.bits.get_bit(2)
    }
    /// Sets the status of the virtual supervisor software interrupt enable.
    #[inline]
    pub fn set_vssie(&mut self, val: bool) {
        self.bits.set_bit(2, val);
    }
    /// Returns the status of the virtual supervisor timer interrupt enable.
    #[inline]
    pub fn vstie(&self) -> bool {
        self.bits.get_bit(6)
    }
    /// Sets the status of the virtual supervisor timer interrupt enable.
    #[inline]
    pub fn set_vstie(&mut self, val: bool) {
        self.bits.set_bit(6, val);
    }
    /// Returns the status of the virtual supervisor external interrupt enable.
    #[inline]
    pub fn vseie(&self) -> bool {
        self.bits.get_bit(10)
    }
    /// Sets the status of the virtual supervisor external interrupt enable.
    #[inline]
    pub fn set_vseie(&mut self, val: bool) {
        self.bits.set_bit(10, val);
    }
    /// Returns the status of the supervisor guest external interrupt enable.
    #[inline]
    pub fn sgeie(&self) -> bool {
        self.bits.get_bit(12)
    }
    /// Sets the status of the supervisor guest external interrupt enable.
    #[inline]
    pub fn set_sgeie(&mut self, val: bool) {
        self.bits.set_bit(12, val);
    }
}

read_csr_as!(Hie, 0x604);
write_csr!(0x604);
set!(0x604);
clear!(0x604);

// bit ops
set_clear_csr!(
    /// Virtual supervisor software interrupt enable.
    , set_vssie, clear_vssie, 1 << 2);
set_clear_csr!(
    /// Virtual supervisor timer interrupt enable.
    , set_vstie, clear_vstie, 1 << 6);
set_clear_csr!(
    /// Virtual supervisor external interrupt enable.
    , set_vseie, clear_vseie, 1 << 10);
set_clear_csr!(
    /// Supervisor guest external interrupt enable.
    , set_sgeie, clear_sgeie, 1 << 12);

// enums
