//! Allocation-free page-table readers for PMU hard-IRQ stack unwinding.

use ax_memory_addr::PhysAddr;

const PTE_VALID: u64 = 1 << 0;
const PTE_TABLE_OR_PAGE: u64 = 1 << 1;
const PTE_ADDR_MASK: u64 = 0x0000_ffff_ffff_f000;
const WORD_SIZE: usize = core::mem::size_of::<u64>();

#[inline]
fn phys_is_ram(pa: usize, len: usize) -> bool {
    let Some(end) = pa.checked_add(len) else {
        return false;
    };
    ax_hal::mem::phys_ram_ranges().iter().any(|&(start, size)| {
        start
            .checked_add(size)
            .is_some_and(|range_end| pa >= start && end <= range_end)
    })
}

#[inline]
fn read_descriptor(table_pa: usize, index: usize) -> Option<u64> {
    if index >= 512 {
        return None;
    }
    let address = table_pa.checked_add(index * WORD_SIZE)?;
    if !phys_is_ram(address, WORD_SIZE) {
        return None;
    }
    let table_va = ax_runtime::hal::mem::phys_to_virt(PhysAddr::from(table_pa)).as_usize();
    // SAFETY: the complete descriptor word is in RAM and `index < 512`.
    Some(unsafe { core::ptr::read_volatile((table_va as *const u64).add(index)) })
}

#[inline]
fn read_phys_word(pa: usize) -> Option<u64> {
    if !phys_is_ram(pa, WORD_SIZE) {
        return None;
    }
    let va = ax_runtime::hal::mem::phys_to_virt(PhysAddr::from(pa)).as_usize();
    // SAFETY: the complete aligned word is in normal RAM through the direct map.
    Some(unsafe { core::ptr::read_volatile(va as *const u64) })
}

fn read_word_via(root_pa: usize, va: usize) -> Option<u64> {
    if !va.is_multiple_of(WORD_SIZE) || va.checked_add(WORD_SIZE).is_none() {
        return None;
    }
    let mut table_pa = (root_pa as u64 & PTE_ADDR_MASK) as usize;
    for level in 0..4usize {
        let shift = 39 - 9 * level;
        let index = (va >> shift) & 0x1ff;
        let pte = read_descriptor(table_pa, index)?;
        if pte & PTE_VALID == 0 {
            return None;
        }
        let table_or_page = pte & PTE_TABLE_OR_PAGE != 0;
        if level < 3 && table_or_page {
            table_pa = (pte & PTE_ADDR_MASK) as usize;
            continue;
        }
        if level == 0 || (level == 3 && !table_or_page) {
            return None;
        }
        let offset_mask = (1usize << shift) - 1;
        let base = (pte & PTE_ADDR_MASK) as usize & !offset_mask;
        return read_phys_word(base | (va & offset_mask));
    }
    None
}

/// Reads one user word through the active `TTBR0_EL1` without dereferencing the
/// untrusted virtual address or taking a page fault.
pub fn read_user_word(va: usize) -> Option<u64> {
    const USER_VA_END: usize = 1 << 48;
    if va == 0 || va.checked_add(WORD_SIZE)? > USER_VA_END {
        return None;
    }
    read_word_via(ax_cpu::asm::read_user_page_table().as_usize(), va)
}

/// Reads one kernel word through `TTBR1_EL1` without directly dereferencing a
/// possibly corrupt frame pointer.
pub fn read_kernel_word(va: usize) -> Option<u64> {
    read_word_via(ax_cpu::asm::read_kernel_page_table().as_usize(), va)
}
