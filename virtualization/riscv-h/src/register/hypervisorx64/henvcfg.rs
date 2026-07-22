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

const CACHE_BLOCK_INVALIDATE_RANGE: core::ops::Range<usize> = 4..6;
const CACHE_BLOCK_CLEAN_FLUSH_BIT: usize = 6;
const CACHE_BLOCK_ZERO_BIT: usize = 7;

/// Guest behavior selected for `CBO.INVAL` by `henvcfg.CBIE`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(usize)]
pub enum CacheBlockInvalidate {
    /// Raise a virtual-instruction exception in VS/VU mode.
    Trap       = 0b00,
    /// Execute the operation with flush semantics.
    Flush      = 0b01,
    /// Reserved architectural encoding.
    Reserved   = 0b10,
    /// Execute with the invalidate/flush semantics selected by lower privilege.
    Invalidate = 0b11,
}

impl From<usize> for CacheBlockInvalidate {
    fn from(value: usize) -> Self {
        match value {
            0b00 => Self::Trap,
            0b01 => Self::Flush,
            0b10 => Self::Reserved,
            0b11 => Self::Invalidate,
            _ => unreachable!("CBIE is a two-bit field"),
        }
    }
}

/// Hypervisor environment configuration register value.
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

    /// Returns the raw register bits.
    #[inline]
    pub const fn bits(self) -> usize {
        self.bits
    }

    /// Returns the `CBO.INVAL` permission and behavior.
    #[inline]
    pub fn cache_block_invalidate(self) -> CacheBlockInvalidate {
        self.bits.get_bits(CACHE_BLOCK_INVALIDATE_RANGE).into()
    }

    /// Selects the `CBO.INVAL` permission and behavior.
    #[inline]
    pub fn set_cache_block_invalidate(&mut self, behavior: CacheBlockInvalidate) {
        self.bits
            .set_bits(CACHE_BLOCK_INVALIDATE_RANGE, behavior as usize);
    }

    /// Returns whether VS/VU may execute `CBO.CLEAN` and `CBO.FLUSH`.
    #[inline]
    pub fn cache_block_clean_flush(self) -> bool {
        self.bits.get_bit(CACHE_BLOCK_CLEAN_FLUSH_BIT)
    }

    /// Controls whether VS/VU may execute `CBO.CLEAN` and `CBO.FLUSH`.
    #[inline]
    pub fn set_cache_block_clean_flush(&mut self, enabled: bool) {
        self.bits.set_bit(CACHE_BLOCK_CLEAN_FLUSH_BIT, enabled);
    }

    /// Returns whether VS/VU may execute `CBO.ZERO`.
    #[inline]
    pub fn cache_block_zero(self) -> bool {
        self.bits.get_bit(CACHE_BLOCK_ZERO_BIT)
    }

    /// Controls whether VS/VU may execute `CBO.ZERO`.
    #[inline]
    pub fn set_cache_block_zero(&mut self, enabled: bool) {
        self.bits.set_bit(CACHE_BLOCK_ZERO_BIT, enabled);
    }

    /// Writes this value to the current hart's `henvcfg` CSR.
    ///
    /// # Safety
    ///
    /// The caller must execute in HS-mode on a hart implementing the H
    /// extension and must own the guest execution environment for that hart.
    #[inline]
    pub unsafe fn write(self) {
        // SAFETY: The caller upholds the CSR privilege and ownership contract.
        unsafe { _write(self.bits) };
    }
}

read_csr_as!(Henvcfg, 0x60a);
write_csr!(0x60a);
