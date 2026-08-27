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

//! Hypervisor environment configuration register.

use bit_field::BitField;
use riscv::{read_csr_as, write_csr};

/// Hypervisor environment configuration register.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Henvcfg {
    bits: usize,
}

impl Henvcfg {
    /// Creates a register value from raw bits.
    #[inline]
    pub const fn from_bits(bits: usize) -> Self {
        Self { bits }
    }

    /// Returns the raw register value.
    #[inline]
    pub const fn bits(self) -> usize {
        self.bits
    }

    /// Returns whether VS-mode may access `stimecmp` directly.
    #[inline]
    pub fn stce(self) -> bool {
        self.bits.get_bit(63)
    }

    /// Controls direct VS-mode `stimecmp` access.
    #[inline]
    pub fn set_stce(&mut self, enabled: bool) {
        self.bits.set_bit(63, enabled);
    }

    /// Writes this value to `henvcfg`.
    ///
    /// # Safety
    ///
    /// The caller must own the current hart's hypervisor CSR context and must
    /// restore the host value before releasing that ownership.
    #[inline]
    pub unsafe fn write(self) {
        unsafe { _write(self.bits) };
    }
}

read_csr_as!(Henvcfg, 0x60a);
write_csr!(0x60a);
