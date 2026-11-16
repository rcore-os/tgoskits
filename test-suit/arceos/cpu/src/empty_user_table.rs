//! An empty user page table, installed for as long as the guard lives.

use std::os::arceos::{modules::ax_hal, sync::IrqSaveGuard};

/// Empty user page table installed on the current CPU.
///
/// Every address in the user range is left unmapped, so a probe of one meets an
/// invalid descriptor rather than another context's translation. IRQs stay
/// disabled for the whole window, and the previous root goes back before the
/// table is released.
pub struct EmptyUserTable {
    saved_root: u64,
    _irq: IrqSaveGuard,
    _table: ax_hal::paging::PageTable,
}

impl EmptyUserTable {
    pub fn install() -> Self {
        let table = ax_hal::paging::PageTable::new(ax_hal::paging::PagingAllocator).unwrap();
        let irq = IrqSaveGuard::new();
        let mut saved_root = 0;
        // SAFETY: this CPU retains its complete TTBR0 value, including ASID, in
        // a register for the duration of the window. The empty table is owned
        // by the returned guard, which restores the value before releasing it.
        unsafe {
            core::arch::asm!("mrs {}, ttbr0_el1", out(reg) saved_root, options(nomem, nostack));
            ax_cpu::mmu::write_user_page_table(table.root_paddr());
        }
        ax_cpu::mmu::flush_tlb(None);
        Self {
            saved_root,
            _irq: irq,
            _table: table,
        }
    }
}

impl Drop for EmptyUserTable {
    fn drop(&mut self) {
        // SAFETY: the value came from TTBR0 on this CPU inside this window, and
        // the empty table is still alive until after this returns.
        unsafe {
            core::arch::asm!("msr ttbr0_el1, {}", in(reg) self.saved_root, options(nostack));
        }
        ax_cpu::mmu::flush_tlb(None);
    }
}
