//! Supervisor translation mode, root and hardware-ASID installation.

pub use riscv::register::satp::{Mode as SatpMode, Satp};

/// Reads the current supervisor translation register, including mode and ASID.
pub fn read_satp() -> Satp {
    riscv::register::satp::read()
}

/// Installs a mode, aligned physical root and hardware ASID, then invalidates
/// all local supervisor translations. No remote hart is synchronized here.
///
/// # Safety
/// The CPU must support `mode`, and the caller must own this hart with IRQs
/// masked. The complete root and mappings must retain current code, stack,
/// CPU anchor and handoff data. The table owner manages remote retirement.
pub unsafe fn install_page_table(mode: SatpMode, space: crate::mmu::HardwareAddressSpace) {
    let root = space.root().as_usize();
    assert!(root & 4095 == 0 && root >> 56 == 0, "invalid SATP root");
    // SAFETY: the owner retained the selected mode and table lifetime.
    unsafe { riscv::register::satp::set(mode, space.hardware_tag() as usize, root >> 12) };
    super::asm::flush_tlb(None);
}
