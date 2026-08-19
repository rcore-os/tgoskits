//! IRQ-safe, allocation-free, no-fault readers for the FP unwinder.
//!
//! The PMU overflow handler unwinds the interrupted stack from hard-IRQ context,
//! where a page fault is fatal (there is no kernel fault-fixup table). A frame
//! pointer taken from a corrupt or mid-prologue register may point anywhere, so
//! the stack must never be dereferenced directly — for *either* user or kernel
//! frames. Both [`read_user_word_nofault`] and [`read_kernel_word_nofault`]
//! instead walk the relevant translation table (`TTBR0_EL1` / `TTBR1_EL1`) by
//! hand against the always-mapped kernel direct map — every access is to a
//! page-table frame or the resolved target page reached through
//! [`phys_to_virt`](ax_runtime::hal::mem::phys_to_virt), never to the input VA —
//! so an unmapped or bogus address yields `None` rather than a fault.
//!
//! This mirrors `PageTable::query` (4 KiB granule, 4-level, 48-bit VA) but is
//! self-contained, takes no locks, and allocates nothing. An IRQ never switches
//! `TTBR0_EL1`/`TTBR1_EL1`, so the interrupted address space stays active while
//! the handler runs.

use ax_memory_addr::PhysAddr;

/// Descriptor is valid / present.
const PTE_VALID: u64 = 1 << 0;
/// Descriptor type bit: `1` = table pointer (levels 0-2) or page (level 3);
/// `0` = block (huge page) descriptor.
const PTE_TABLE_OR_PAGE: u64 = 1 << 1;
/// Output-address field of a descriptor: bits `[47:12]`. Masks off the low
/// attribute bits and the high `TTBR0` ASID bits.
const PTE_ADDR_MASK: u64 = 0x0000_ffff_ffff_f000;

/// Whether physical address `pa` lies in normal, linear-mapped RAM.
///
/// The kernel linear map also aliases device MMIO (`Device-nGnRE`), so
/// `phys_to_virt` alone does not distinguish RAM from a device register. Every
/// physical the walk is about to dereference (table frames *and* the resolved
/// leaf) is checked against the RAM ranges first, so a valid page-table entry
/// whose output address is device MMIO — reachable via a user `mmap` of an
/// accelerator register window (RGA/NPU/JPU) plus a wild frame pointer, or a
/// concurrently-torn descriptor on another CPU — can never trigger a live MMIO
/// read (device side effect) or a data abort (panic) from hard-IRQ context.
/// `phys_ram_ranges` is a tiny `'static` slice (lock-free, IRQ-safe).
#[inline]
fn phys_is_ram(pa: usize) -> bool {
    ax_hal::mem::phys_ram_ranges()
        .iter()
        .any(|&(start, size)| pa >= start && pa - start < size)
}

/// Reads the raw descriptor at `index` of the page-table frame at physical
/// address `table_pa`, through the direct map. `None` if `table_pa` is not RAM
/// (e.g. a stale/torn table pointer), so the deref cannot fault.
#[inline]
fn read_descriptor(table_pa: usize, index: usize) -> Option<u64> {
    if !phys_is_ram(table_pa) {
        return None;
    }
    let table_va = ax_runtime::hal::mem::phys_to_virt(PhysAddr::from(table_pa)).as_usize();
    // SAFETY: `table_pa` is confirmed RAM, so `table_va` is the direct-map virtual
    // address of a mapped page-table frame; `index < 512` keeps the read inside the
    // 4 KiB frame. Cannot fault.
    Some(unsafe { *((table_va as *const u64).add(index)) })
}

/// Reads the 64-bit word at physical address `pa` through the direct map. `None`
/// if `pa` is not RAM (e.g. a device-MMIO leaf), so the read has no side effect
/// and cannot fault.
#[inline]
fn read_phys_word(pa: usize) -> Option<u64> {
    if !phys_is_ram(pa) {
        return None;
    }
    let va = ax_runtime::hal::mem::phys_to_virt(PhysAddr::from(pa)).as_usize();
    // SAFETY: `pa` is confirmed normal RAM, so `va` is the direct-map virtual
    // address of a mapped page. The caller only passes 8-aligned addresses, so the
    // 8-byte read stays within one page (4096 is a multiple of 8). The input
    // virtual address itself is never dereferenced.
    Some(unsafe { *(va as *const u64) })
}

/// Reads the 64-bit word at virtual address `va` by walking the translation
/// table rooted at physical address `root_pa`, entirely through the direct map.
///
/// Returns the word at the resolved physical address if `va` maps to a present
/// page, or `None` for any not-present, reserved, or malformed translation. Never
/// dereferences `va` itself, so it cannot fault. `va` must be 8-byte aligned (an
/// 8-byte read at an 8-aligned address never straddles a page).
fn read_word_via(root_pa: usize, va: usize) -> Option<u64> {
    // Root frame (mask off any ASID + attribute bits carried in the TTBR value).
    let mut table_pa = (root_pa as u64 & PTE_ADDR_MASK) as usize;

    // 4-level walk: level 0 indexes VA[47:39], … level 3 indexes VA[20:12].
    for level in 0..4usize {
        let shift = 39 - 9 * level;
        let index = (va >> shift) & 0x1ff;
        let pte = read_descriptor(table_pa, index)?;
        if pte & PTE_VALID == 0 {
            return None;
        }
        let table_or_page = pte & PTE_TABLE_OR_PAGE != 0;
        if level < 3 {
            if table_or_page {
                // Table descriptor: descend.
                table_pa = (pte & PTE_ADDR_MASK) as usize;
                continue;
            }
            // Block (huge page) descriptor. A block is not permitted at level 0.
            if level == 0 {
                return None;
            }
        } else if !table_or_page {
            // A level-3 descriptor with the type bit clear is reserved/invalid.
            return None;
        }

        // Leaf reached (4 KiB page at level 3, 2 MiB block at level 2, 1 GiB block
        // at level 1). Compose the output physical address for `va`.
        let offset_mask = (1usize << shift) - 1;
        let base = (pte & PTE_ADDR_MASK) as usize & !offset_mask;
        return read_phys_word(base | (va & offset_mask));
    }

    // Unreachable: the level-3 iteration always returns.
    None
}

/// Reads the 64-bit word at *user* virtual address `va` without ever faulting.
///
/// Walks the active user translation table (`TTBR0_EL1`). See [`read_word_via`].
/// Allocation-free, lock-free, and safe from hard-IRQ context; an IRQ never
/// switches `TTBR0_EL1`, so the interrupted user address space stays active.
pub fn read_user_word_nofault(va: usize) -> Option<u64> {
    read_word_via(ax_cpu::asm::read_user_page_table().as_usize(), va)
}

/// Reads the 64-bit word at *kernel* virtual address `va` without ever faulting.
///
/// Walks the kernel translation table (`TTBR1_EL1`) the same way as
/// [`read_user_word_nofault`]. Used by the kernel FP unwinder so a corrupt or
/// stale in-kernel frame pointer that slips past the range/alignment guards and
/// points at an unmapped kernel VA yields `None` instead of an unrecoverable data
/// abort (kernel panic) in hard-IRQ context — kernel stacks are mapped, so a
/// legitimate frame still resolves and is read through the direct map.
pub fn read_kernel_word_nofault(va: usize) -> Option<u64> {
    read_word_via(ax_cpu::asm::read_kernel_page_table().as_usize(), va)
}
