// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors.
// Linux v7.2.3 scatterlist helpers used by DRM GEM memory paths.

use alloc::vec::Vec;
use core::{
    ffi::{c_int, c_ulong, c_void},
    mem::size_of,
    ptr,
};

use crate::{
    config::{EINVAL, ENOMEM, PAGE_SHIFT, PAGE_SIZE},
    memory::{kfree, kmalloc},
    shmem::try_page_to_phys,
    types::{Page, Scatterlist, SgTable},
};

/// Iterator corresponding to a page walk of the mapped segments in an sg_table.
pub struct SgTablePages {
    sg: *mut Scatterlist,
    remaining: u32,
}

impl Iterator for SgTablePages {
    type Item = *mut Page;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 || self.sg.is_null() {
            return None;
        }
        let current = self.sg;
        // SAFETY: `current` is non-null and `self.remaining > 0`. Per iterator invariant,
        // `current` points to a live `Scatterlist` in the table; `sg_next` advances to the next entry
        // and `sg_page` retrieves its decoded page pointer.
        unsafe {
            self.sg = sg_next(current);
            self.remaining -= 1;
            Some(sg_page(current))
        }
    }
}

/// Walk each DMA-mapped sg segment's starting page.
///
/// # Safety
/// `table` must point to a live initialized table whose `sgl` entries and
/// pages remain valid for the lifetime of the returned iterator.
pub unsafe fn sg_table_pages(table: *mut SgTable) -> SgTablePages {
    SgTablePages {
        // SAFETY: Caller guarantees `table` points to a live initialized SgTable.
        sg: unsafe { (*table).sgl },
        // SAFETY: Caller guarantees `table` points to a live initialized SgTable.
        remaining: unsafe { (*table).nents },
    }
}

/// Linux `dma_max_mapping_size()`: with VT-d translation and no swiotlb
/// bounce buffer on this target, no per-device mapping cap exists.
#[unsafe(export_name = "dma_max_mapping_size")]
pub extern "C" fn dma_max_mapping_size(_dev: *mut c_void) -> usize {
    usize::MAX
}

/// Linux `sg_alloc_table(table, nents, gfp)`. The entries form one array,
/// terminated by the end marker. Linux chains arrays above one page of
/// entries; `sg_next`-based walkers see the same sequence either way.
///
/// # Safety
/// `table` must point to a writable `sg_table`.
#[unsafe(export_name = "sg_alloc_table")]
pub unsafe extern "C" fn sg_alloc_table(table: *mut SgTable, nents: u32, gfp_mask: u32) -> c_int {
    if table.is_null() || nents == 0 {
        return -EINVAL;
    }
    let Some(bytes) = (nents as usize).checked_mul(size_of::<Scatterlist>()) else {
        return -EINVAL;
    };
    let sgl = kmalloc(bytes, gfp_mask).cast::<Scatterlist>();
    if sgl.is_null() {
        return -ENOMEM;
    }
    // SAFETY: `sgl` is a non-null, freshly allocated buffer of `bytes` bytes from kmalloc,
    // properly aligned for `Scatterlist` with capacity for `nents` entries. `table` is
    // verified non-null and writable per caller contract.
    unsafe {
        ptr::write_bytes(sgl.cast::<u8>(), 0, bytes);
        sg_mark_end(sgl.add(nents as usize - 1));
        (*table).sgl = sgl;
        (*table).nents = nents;
        (*table).orig_nents = nents;
    }
    0
}

/// Linux `sg_free_table(table)`: releases the entry array and clears the table.
///
/// # Safety
/// `table` must have been filled by one of the `sg_alloc_table*` calls.
#[unsafe(export_name = "sg_free_table")]
pub unsafe extern "C" fn sg_free_table(table: *mut SgTable) {
    if table.is_null() {
        return;
    }
    // SAFETY: `table` is verified non-null. Per function safety contract, `table` was populated
    // by `sg_alloc_table*`, so `(*table).sgl` is either null or was allocated via kmalloc and can be freed.
    unsafe {
        if !(*table).sgl.is_null() {
            kfree((*table).sgl);
        }
        (*table).sgl = ptr::null_mut();
        (*table).nents = 0;
        (*table).orig_nents = 0;
    }
}

/// One merged scatterlist segment before the entry array is allocated.
struct Segment {
    page: *mut Page,
    offset: u32,
    length: u32,
    physical_end: usize,
}

/// Linux `sg_alloc_table_from_pages_segment()`: build a table covering the
/// byte range `[offset, offset + size)` of `n_pages` pages. Physically
/// contiguous neighbours merge into one segment, and no segment exceeds
/// `max_segment` bytes.
///
/// # Safety
/// `pages` must hold `n_pages` registered page identities.
#[unsafe(export_name = "sg_alloc_table_from_pages_segment")]
pub unsafe extern "C" fn sg_alloc_table_from_pages_segment(
    table: *mut SgTable,
    pages: *mut *mut Page,
    n_pages: u32,
    offset: u32,
    size: c_ulong,
    max_segment: u32,
    gfp_mask: u32,
) -> c_int {
    let page_size = PAGE_SIZE;
    if table.is_null()
        || pages.is_null()
        || n_pages == 0
        || max_segment == 0
        || size == 0
        || offset as usize >= page_size
    {
        return -EINVAL;
    }
    let Some(end) = (offset as usize).checked_add(size as usize) else {
        return -EINVAL;
    };
    if end > (n_pages as usize) * page_size {
        return -EINVAL;
    }

    let mut segments: Vec<Segment> = Vec::new();
    for index in 0..n_pages as usize {
        let page_start = index * page_size;
        if page_start >= end {
            break;
        }
        let lo = (offset as usize).max(page_start);
        let hi = end.min(page_start + page_size);
        if lo >= hi {
            continue;
        }
        // SAFETY: `index < n_pages`, and per caller contract `pages` points to an array of `n_pages` registered Page pointers.
        let page = unsafe { pages.add(index).read() };
        if page.is_null() {
            return -EINVAL;
        }
        // SAFETY: `page` is verified non-null and was obtained from the caller-provided `pages` array.
        let Some(page_physical) = (unsafe { try_page_to_phys(page) }) else {
            return -EINVAL;
        };
        let mut in_page = lo - page_start;
        let mut remaining = hi - lo;
        while remaining > 0 {
            let physical = page_physical + in_page;
            let mergeable = segments.last().is_some_and(|last| {
                last.physical_end == physical && (last.length as usize) < max_segment as usize
            });
            if mergeable {
                let last = segments.last_mut().expect("mergeable segment exists");
                let room = max_segment as usize - last.length as usize;
                let take = remaining.min(room);
                last.length += take as u32;
                last.physical_end = physical + take;
                in_page += take;
                remaining -= take;
            } else {
                let take = remaining.min(max_segment as usize);
                segments.push(Segment {
                    page,
                    offset: in_page as u32,
                    length: take as u32,
                    physical_end: physical + take,
                });
                in_page += take;
                remaining -= take;
            }
        }
    }

    let Ok(count) = u32::try_from(segments.len()) else {
        return -EINVAL;
    };
    // SAFETY: `table` is non-null and writable per caller contract; `count` is the computed number of merged segments.
    let status = unsafe { sg_alloc_table(table, count, gfp_mask) };
    if status != 0 {
        return status;
    }
    // SAFETY: `table` was allocated with `count` entries by `sg_alloc_table` above.
    // Iterating over `segments` writes each segment to a valid `Scatterlist` within `(*table).sgl`,
    // and `sg_mark_end` marks the final element.
    unsafe {
        let mut sg = (*table).sgl;
        for segment in &segments {
            (*sg).page_link = segment.page as usize;
            (*sg).offset = segment.offset;
            (*sg).length = segment.length;
            sg = sg.add(1);
        }
        sg_mark_end((*table).sgl.add(count as usize - 1));
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::GFP_KERNEL,
        shmem::{alloc_pages, free_pages, pfn_to_page, try_page_to_phys},
    };

    fn empty_table() -> SgTable {
        SgTable {
            sgl: ptr::null_mut(),
            nents: 0,
            orig_nents: 0,
        }
    }

    #[test]
    fn contiguous_pages_merge_and_split_at_max_segment() {
        // SAFETY: Test allocates compound pages using alloc_pages, constructs page pointers via pfn_to_page,
        // and tests scatterlist segment allocation and freeing on live data.
        unsafe {
            let head = alloc_pages(GFP_KERNEL, 2);
            assert!(!head.is_null());
            let base = try_page_to_phys(head).expect("compound head is registered");
            let mut pages = [ptr::null_mut::<Page>(); 4];
            for (index, slot) in pages.iter_mut().enumerate() {
                *slot = pfn_to_page(((base >> PAGE_SHIFT) + index) as c_ulong);
                assert!(!slot.is_null());
            }

            let mut table = empty_table();
            let whole = 4 * PAGE_SIZE as c_ulong;
            assert_eq!(
                sg_alloc_table_from_pages_segment(
                    &mut table,
                    pages.as_mut_ptr(),
                    4,
                    0,
                    whole,
                    u32::MAX,
                    GFP_KERNEL,
                ),
                0
            );
            assert_eq!(table.nents, 1);
            assert_eq!((*table.sgl).length as usize, 4 * PAGE_SIZE);
            assert_eq!(sg_page(table.sgl), pages[0]);
            sg_free_table(&mut table);
            assert!(table.sgl.is_null());

            assert_eq!(
                sg_alloc_table_from_pages_segment(
                    &mut table,
                    pages.as_mut_ptr(),
                    4,
                    0,
                    whole,
                    2 * PAGE_SIZE as u32,
                    GFP_KERNEL,
                ),
                0
            );
            assert_eq!(table.nents, 2);
            assert_eq!((*table.sgl).length as usize, 2 * PAGE_SIZE);
            assert_eq!((*sg_next(table.sgl)).length as usize, 2 * PAGE_SIZE);
            sg_free_table(&mut table);

            // A window starting mid-page keeps the byte offset on the first segment.
            assert_eq!(
                sg_alloc_table_from_pages_segment(
                    &mut table,
                    pages.as_mut_ptr(),
                    4,
                    1000,
                    2 * PAGE_SIZE as c_ulong,
                    u32::MAX,
                    GFP_KERNEL,
                ),
                0
            );
            assert_eq!(table.nents, 1);
            assert_eq!((*table.sgl).offset, 1000);
            assert_eq!((*table.sgl).length as usize, 2 * PAGE_SIZE);
            sg_free_table(&mut table);

            // Ranges past the supplied pages are refused.
            assert_eq!(
                sg_alloc_table_from_pages_segment(
                    &mut table,
                    pages.as_mut_ptr(),
                    4,
                    0,
                    5 * PAGE_SIZE as c_ulong,
                    u32::MAX,
                    GFP_KERNEL,
                ),
                -EINVAL
            );

            free_pages(head, 2);
        }
    }

    #[test]
    fn sg_alloc_table_rejects_zero_and_marks_end() {
        // SAFETY: Stack-allocated `table` is passed to `sg_alloc_table` and `sg_free_table`.
        unsafe {
            let mut table = empty_table();
            assert_eq!(sg_alloc_table(&mut table, 0, GFP_KERNEL), -EINVAL);
            assert_eq!(sg_alloc_table(&mut table, 3, GFP_KERNEL), 0);
            assert_eq!(table.nents, 3);
            assert_eq!(table.orig_nents, 3);
            let last = table.sgl.add(2);
            assert!(sg_next(last).is_null());
            sg_free_table(&mut table);
        }
    }
}
pub const SG_CHAIN: usize = 1;

pub const SG_END: usize = 2;

pub const SG_PAGE_LINK_MASK: usize = SG_CHAIN | SG_END;

/// # Safety
/// `sg` must point to a live initialized scatterlist entry whose encoded page
/// pointer is valid.
#[inline]
pub unsafe fn sg_page(sg: *mut Scatterlist) -> *mut Page {
    // SAFETY: Per function safety contract, `sg` points to a live initialized `Scatterlist`.
    unsafe { ((*sg).page_link & !SG_PAGE_LINK_MASK) as *mut Page }
}

/// Linux `sg_mark_end()` header helper: set the terminal bit and clear a
/// possible chain marker without disturbing the encoded page pointer.
///
/// # Safety
/// `sg` must point to a live writable scatterlist entry in an initialized
/// table.
#[inline]
pub unsafe fn sg_mark_end(sg: *mut Scatterlist) {
    // SAFETY: Per function safety contract, `sg` points to a live, writable `Scatterlist`.
    unsafe {
        (*sg).page_link |= SG_END;
        (*sg).page_link &= !SG_CHAIN;
    }
}

/// Linux `sg_set_folio()` header helper. `struct folio` begins with its
/// embedded `struct page`; the caller supplies the source `size_t` length and
/// offset, which Linux stores in the unsigned-int scatterlist fields.
///
/// # Safety
/// `sg` must point to a live writable entry and `folio` to a live folio whose
/// first field is its page; the folio must outlive uses of the entry.
#[inline]
pub unsafe fn sg_set_folio<T>(sg: *mut Scatterlist, folio: *mut T, len: usize, offset: usize) {
    // SAFETY: Per safety contract, `sg` is writable and `folio` begins with a live `Page`.
    unsafe {
        let page = folio.cast::<Page>();
        (*sg).page_link = ((*sg).page_link & SG_PAGE_LINK_MASK) | page as usize;
        (*sg).offset = offset as u32;
        (*sg).length = len as u32;
    }
}

/// # Safety
/// `sg` must point to a live initialized scatterlist entry.
#[inline]
pub unsafe fn sg_dma_address(sg: *mut Scatterlist) -> u64 {
    // SAFETY: Per function safety contract, `sg` points to a live initialized `Scatterlist`.
    unsafe { (*sg).dma_address }
}

/// # Safety
/// `sg` must point to a live initialized scatterlist entry.
#[inline]
pub unsafe fn sg_dma_len(sg: *mut Scatterlist) -> u32 {
    // SAFETY: Per function safety contract, `sg` points to a live initialized `Scatterlist`.
    unsafe { (*sg).dma_length }
}

/// # Safety
/// `sg` must point to a live initialized scatterlist entry.
#[inline]
pub unsafe fn sg_page_count(sg: *mut Scatterlist) -> u32 {
    // SAFETY: Per function safety contract, `sg` points to a live initialized `Scatterlist`.
    unsafe { (*sg).length >> PAGE_SHIFT }
}

/// # Safety
/// `sg` must point to a live initialized scatterlist entry.
#[inline]
pub unsafe fn sg_dma_page_count(sg: *mut Scatterlist) -> u32 {
    // SAFETY: Per function safety contract, `sg` points to a live initialized `Scatterlist`.
    unsafe { (*sg).dma_length >> PAGE_SHIFT }
}

/// # Safety
/// `sg` must point to a live initialized entry marked as a chain link.
#[inline]
pub unsafe fn sg_chain_ptr(sg: *mut Scatterlist) -> *mut Scatterlist {
    // SAFETY: Per function safety contract, `sg` points to a live entry marked as a chain link.
    unsafe { ((*sg).page_link & !SG_PAGE_LINK_MASK) as *mut Scatterlist }
}

/// # Safety
/// `sg` must point to a live initialized entry, and any following or chained
/// entry it denotes must remain live in the same table.
#[inline]
pub unsafe fn sg_next(sg: *mut Scatterlist) -> *mut Scatterlist {
    // SAFETY: Per function safety contract, `sg` points to a live initialized entry,
    // and any following or chained entry remains valid in the table.
    unsafe {
        if sg.is_null() || (*sg).page_link & SG_END != 0 {
            return ptr::null_mut();
        }
        let next = sg.add(1);
        if (*next).page_link & SG_CHAIN != 0 {
            sg_chain_ptr(next)
        } else {
            next
        }
    }
}

// ____sg_next() deliberately does not stop at SG_END.
/// # Safety
/// `sg` must point to a live entry with a live following or chained entry in
/// the same scatterlist allocation.
#[inline]
pub unsafe fn ____sg_next(sg: *mut Scatterlist) -> *mut Scatterlist {
    // SAFETY: Per function safety contract, `sg` points to a live entry with a valid following or chained entry.
    unsafe {
        let next = sg.add(1);
        if (*next).page_link & SG_CHAIN != 0 {
            sg_chain_ptr(next)
        } else {
            next
        }
    }
}
