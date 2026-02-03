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

//! Hypervisor Exception Delegation Register.
//!
//! The `hedeleg` register controls which exceptions are delegated from HS-mode to VS-mode.
//! When a bit is set in this register, the corresponding exception will trap to VS-mode
//! instead of HS-mode when occurring in VS-mode or VU-mode.
//!
//! This register enables efficient virtualization by allowing guests to handle
//! common exceptions (like page faults) directly without hypervisor intervention.
//! Exception codes correspond to standard RISC-V exception cause values.

use bit_field::BitField;
use riscv::{clear, read_csr_as, set, set_clear_csr, write_csr};

/// Hypervisor Trap Delegation Registers.
#[derive(Copy, Clone, Debug)]
pub struct Hedeleg {
    bits: usize,
}

impl Hedeleg {
    /// Returns the raw bits of the register.
    #[inline]
    pub fn bits(&self) -> usize {
        self.bits
    }
    /// Creates a register value from raw bits.
    #[inline]
    pub fn from_bits(x: usize) -> Self {
        Hedeleg { bits: x }
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
    /// Returns the instruction address misaligned exception delegation.
    #[inline]
    pub fn ex0(&self) -> bool {
        self.bits.get_bit(0)
    }
    /// Sets the instruction address misaligned exception delegation.
    #[inline]
    pub fn set_ex0(&mut self, val: bool) {
        self.bits.set_bit(0, val);
    }
    /// Returns the instruction access fault exception delegation.
    #[inline]
    pub fn ex1(&self) -> bool {
        self.bits.get_bit(1)
    }
    /// Sets the instruction access fault exception delegation.
    #[inline]
    pub fn set_ex1(&mut self, val: bool) {
        self.bits.set_bit(1, val);
    }
    /// Returns the illegal instruction exception delegation.
    #[inline]
    pub fn ex2(&self) -> bool {
        self.bits.get_bit(2)
    }
    /// Sets the illegal instruction exception delegation.
    #[inline]
    pub fn set_ex2(&mut self, val: bool) {
        self.bits.set_bit(2, val);
    }
    /// Returns the breakpoint exception delegation.
    #[inline]
    pub fn ex3(&self) -> bool {
        self.bits.get_bit(3)
    }
    /// Sets the breakpoint exception delegation.
    #[inline]
    pub fn set_ex3(&mut self, val: bool) {
        self.bits.set_bit(3, val);
    }
    /// Returns the load address misaligned exception delegation.
    #[inline]
    pub fn ex4(&self) -> bool {
        self.bits.get_bit(4)
    }
    /// Sets the load address misaligned exception delegation.
    #[inline]
    pub fn set_ex4(&mut self, val: bool) {
        self.bits.set_bit(4, val);
    }
    /// Returns the load access fault exception delegation.
    #[inline]
    pub fn ex5(&self) -> bool {
        self.bits.get_bit(5)
    }
    /// Sets the load access fault exception delegation.
    #[inline]
    pub fn set_ex5(&mut self, val: bool) {
        self.bits.set_bit(5, val);
    }
    /// Returns the store/AMO address misaligned exception delegation.
    #[inline]
    pub fn ex6(&self) -> bool {
        self.bits.get_bit(6)
    }
    /// Sets the store/AMO address misaligned exception delegation.
    #[inline]
    pub fn set_ex6(&mut self, val: bool) {
        self.bits.set_bit(6, val);
    }
    /// Returns the store/AMO access fault exception delegation.
    #[inline]
    pub fn ex7(&self) -> bool {
        self.bits.get_bit(7)
    }
    /// Sets the store/AMO access fault exception delegation.
    #[inline]
    pub fn set_ex7(&mut self, val: bool) {
        self.bits.set_bit(7, val);
    }
    /// Returns the environment call exception delegation.
    #[inline]
    pub fn ex8(&self) -> bool {
        self.bits.get_bit(8)
    }
    /// Sets the environment call exception delegation.
    #[inline]
    pub fn set_ex8(&mut self, val: bool) {
        self.bits.set_bit(8, val);
    }
    /// Returns the instruction page fault exception delegation.
    #[inline]
    pub fn ex12(&self) -> bool {
        self.bits.get_bit(12)
    }
    /// Sets the instruction page fault exception delegation.
    #[inline]
    pub fn set_ex12(&mut self, val: bool) {
        self.bits.set_bit(12, val);
    }
    /// Returns the load page fault exception delegation.
    #[inline]
    pub fn ex13(&self) -> bool {
        self.bits.get_bit(13)
    }
    /// Sets the load page fault exception delegation.
    #[inline]
    pub fn set_ex13(&mut self, val: bool) {
        self.bits.set_bit(13, val);
    }
    /// Returns the store/AMO page fault exception delegation.
    #[inline]
    pub fn ex15(&self) -> bool {
        self.bits.get_bit(15)
    }
    /// Sets the store/AMO page fault exception delegation.
    #[inline]
    pub fn set_ex15(&mut self, val: bool) {
        self.bits.set_bit(15, val);
    }
}

read_csr_as!(Hedeleg, 0x602);
write_csr!(0x602);
set!(0x602);
clear!(0x602);

// bit ops
set_clear_csr!(
    /// Instruction address misaligned enable.
    , set_ex0, clear_ex0, 1 << 0);
set_clear_csr!(
    /// Instruction access fault enable.
    , set_ex1, clear_ex1, 1 << 1);
set_clear_csr!(
    /// Illegal instruction enable.
    , set_ex2, clear_ex2, 1 << 2);
set_clear_csr!(
    /// Breakpoint enable.
    , set_ex3, clear_ex3, 1 << 3);
set_clear_csr!(
    /// Load address misaligned enable.
    , set_ex4, clear_ex4, 1 << 4);
set_clear_csr!(
    /// Load access fault enable.
    , set_ex5, clear_ex5, 1 << 5);
set_clear_csr!(
    /// Store/AMO address misaligned enable.
    , set_ex6, clear_ex6, 1 << 6);
set_clear_csr!(
    /// Store/AMO access fault enable.
    , set_ex7, clear_ex7, 1 << 7);
set_clear_csr!(
    /// Environment call enable.
    , set_ex8, clear_ex8, 1 << 8);
set_clear_csr!(
    /// Instruction page fault enable.
    , set_ex12, clear_ex12, 1 << 12);
set_clear_csr!(
    /// Load page fault enable.
    , set_ex13, clear_ex13, 1 << 13);
set_clear_csr!(
    /// Store/AMO page fault enable.
    , set_ex15, clear_ex15, 1 << 15);

// enums
