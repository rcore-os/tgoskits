//! Per-page metadata stored in the external metadata region.
//!
//! Each page frame in the heap has a corresponding [`PageMeta`] entry.
//! Free pages are linked together via intrusive doubly-linked lists using PFN indices.

/// Sentinel value indicating "no page" in free-list links.
pub const PFN_NONE: u32 = u32::MAX;

/// Page state flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PageFlags {
    /// Page is free and sits in a buddy free list.
    Free      = 0,
    /// Page is allocated (head page of a buddy block).
    Allocated = 1,
    /// Page is used as a slab page.
    Slab      = 2,
}

/// Metadata for a single page frame (12 bytes).
///
/// Head pages carry the `order` of the entire block.
/// Tail pages within a buddy block are marked `Allocated` (or `Slab`) with order 0.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct PageMeta {
    /// Current state of this page.
    pub flags: PageFlags,
    /// Order of the block (only meaningful on head pages).
    pub order: u8,
    /// Reserved padding.
    pub _pad: u16,
    /// Previous PFN in the same-order free list (`PFN_NONE` if head or not free).
    pub prev: u32,
    /// Next PFN in the same-order free list (`PFN_NONE` if tail or not free).
    pub next: u32,
}

const _: () = assert!(core::mem::size_of::<PageMeta>() == 12);

impl Default for PageMeta {
    fn default() -> Self {
        Self::new()
    }
}

impl PageMeta {
    /// Create a zeroed (free, order-0) page meta.
    pub const fn new() -> Self {
        Self {
            flags: PageFlags::Free,
            order: 0,
            _pad: 0,
            prev: PFN_NONE,
            next: PFN_NONE,
        }
    }
}

// ---------------------------------------------------------------------------
// Free-list helpers operating on a `*mut PageMeta` array + head array
// ---------------------------------------------------------------------------

/// Push `pfn` onto the front of `free_lists[order]`.
///
/// # Safety
/// `meta` must point to an array with at least `pfn + 1` entries.
/// `pfn` must not already be in any free list.
#[inline]
pub unsafe fn free_list_push(meta: *mut PageMeta, free_lists: &mut [u32], pfn: u32, order: usize) {
    unsafe {
        let old_head = free_lists[order];
        let m = &mut *meta.add(pfn as usize);
        m.prev = PFN_NONE;
        m.next = old_head;
        if old_head != PFN_NONE {
            (*meta.add(old_head as usize)).prev = pfn;
        }
        free_lists[order] = pfn;
    }
}

/// Pop the first PFN from `free_lists[order]`, returning `PFN_NONE` if empty.
///
/// # Safety
/// `meta` must be a valid metadata array.
#[inline]
pub unsafe fn free_list_pop(meta: *mut PageMeta, free_lists: &mut [u32], order: usize) -> u32 {
    unsafe {
        let head = free_lists[order];
        if head == PFN_NONE {
            return PFN_NONE;
        }
        let m = &mut *meta.add(head as usize);
        let next = m.next;
        m.prev = PFN_NONE;
        m.next = PFN_NONE;
        if next != PFN_NONE {
            (*meta.add(next as usize)).prev = PFN_NONE;
        }
        free_lists[order] = next;
        head
    }
}

/// Remove `pfn` from the free list at `order`.
///
/// # Safety
/// `pfn` must currently be in `free_lists[order]`.
#[inline]
pub unsafe fn free_list_remove(
    meta: *mut PageMeta,
    free_lists: &mut [u32],
    pfn: u32,
    order: usize,
) {
    unsafe {
        let m = &*meta.add(pfn as usize);
        let prev = m.prev;
        let next = m.next;

        if prev != PFN_NONE {
            (*meta.add(prev as usize)).next = next;
        } else {
            // pfn was the head
            free_lists[order] = next;
        }
        if next != PFN_NONE {
            (*meta.add(next as usize)).prev = prev;
        }

        let m = &mut *meta.add(pfn as usize);
        m.prev = PFN_NONE;
        m.next = PFN_NONE;
    }
}
