//! RISC-V G-stage root encoding and geometry.

use super::{GuestBinding, Vcpu, VirtualizationError};
use crate::{PhysAddr, virtualization::StateMemory};

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

impl<M: StateMemory> Vcpu<M> {
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

impl GuestBinding {
    /// Installs the saved G-stage root into the currently bound guest bank.
    ///
    /// # Safety
    /// Execute on this binding's original hart with the CPU pin and exclusive
    /// HS guest-bank ownership held, and the H extension enabled. `registers`
    /// must be this binding's validated image with a supported G-stage mode.
    /// Retain the new root and its backing through every subsequent use, and
    /// the old root until all harts have retired its translations.
    pub unsafe fn sync_page_table<M: StateMemory>(&mut self, registers: &Vcpu<M>) {
        let _irqs = super::binding::LocalIrqs::disable();
        // SAFETY: the binding retains exclusive access to this hart's guest CSR
        // bank; the caller retains the validated root and all referenced memory.
        unsafe {
            core::arch::asm!(
                ".option push",
                ".option arch, +h",
                "csrw hgatp, {root}",
                "hfence.gvma",
                ".option pop",
                root = in(reg) registers.virtual_hs_csrs.hgatp,
                options(nostack)
            );
        }
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

/// Invalidates every VS-stage translation belonging to `vmid` on this hart.
/// Preserves the installed G-stage root and its VMID, including in an IPI
/// interrupting a loaded guest binding. No guest memory is accessed here.
///
/// # Safety
/// Execute in HS mode with H enabled, without migration or concurrent guest
/// execution on this hart. `vmid` must be implemented by the current hart.
/// Remote completion and backing-memory retirement belong to the caller.
pub unsafe fn invalidate_vsstage_translations(vmid: u16) {
    assert!(vmid < (1 << 14));
    let _irqs = super::binding::LocalIrqs::disable();
    // SAFETY: the caller excludes migration and guest execution. IRQ exclusion
    // keeps the temporary VMID private; the root PPN/mode are preserved and
    // restored before any interrupt or guest can observe the register bank.
    unsafe {
        core::arch::asm!(
            ".option push",
            ".option arch, +h",
            "csrr {saved}, hgatp",
            "and {temporary}, {saved}, {keep}",
            "or {temporary}, {temporary}, {vmid}",
            "beq {temporary}, {saved}, 2f",
            "csrw hgatp, {temporary}",
            "2:",
            "hfence.vvma",
            "beq {temporary}, {saved}, 3f",
            "csrw hgatp, {saved}",
            "3:",
            ".option pop",
            saved = out(reg) _,
            temporary = out(reg) _,
            keep = in(reg) !(0x3fffusize << 44),
            vmid = in(reg) usize::from(vmid) << 44,
            options(nostack)
        );
    }
}
