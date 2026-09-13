//! RISC-V G-stage root encoding and geometry.

use super::{Vcpu, VirtualizationError};
use crate::PhysAddr;

/// Implemented G-stage translation mode, including the widened root index.
#[repr(usize)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GStageMode {
    /// Three levels with an 11-bit root index and 41-bit guest physical input.
    Sv39x4 = 8,
    /// Four levels with an 11-bit root index and 50-bit guest physical input.
    Sv48x4 = 9,
}

impl GStageMode {
    /// Returns the number of page-table levels for this mode.
    pub const fn levels(self) -> usize {
        match self {
            Self::Sv39x4 => 3,
            Self::Sv48x4 => 4,
        }
    }
    /// Returns the guest physical input address width.
    pub const fn guest_address_bits(self) -> usize {
        match self {
            Self::Sv39x4 => 41,
            Self::Sv48x4 => 50,
        }
    }
}

impl Vcpu {
    /// Encodes a G-stage root using VMID zero, without installing it.
    /// Rejects roots that violate 16-KiB alignment or the 44-bit PPN field.
    /// Memory ownership, mode support and mapping lifetime belong to the binder.
    pub fn set_page_table(
        &mut self,
        root: PhysAddr,
        mode: GStageMode,
    ) -> Result<(), VirtualizationError> {
        let address = root.as_usize();
        if address & 0x3fff != 0 || address >> 56 != 0 {
            return Err(VirtualizationError::InvalidRoot);
        }
        self.virtual_hs_csrs.hgatp = ((mode as usize) << 60) | (address >> 12);
        Ok(())
    }
}

/// Orders this hart's G-stage table stores and invalidates cached translations
/// for every guest physical address and VMID. Remote harts require an owner
/// rendezvous or firmware remote fence before old mappings can be retired.
///
/// # Safety
/// Execute in HS mode with the H extension enabled. The caller serializes
/// table mutation and retains the affected table/backing memory until all
/// participating harts have completed the required invalidations.
pub unsafe fn invalidate_gstage_translations() {
    // SAFETY: the caller retains the privileged hart and table ownership window.
    unsafe {
        core::arch::asm!(
            ".option push",
            ".option arch, +h",
            "hfence.gvma",
            ".option pop",
            options(nostack)
        );
    }
}
