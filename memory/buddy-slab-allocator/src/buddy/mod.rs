//! Buddy page allocator — page-metadata-based with intrusive free lists.
//!
//! The allocator manages one or more contiguous virtual address ranges ("sections").
//! Each section stores its own [`BuddySection`] descriptor and [`PageMeta`] array
//! in the caller-provided region prefix, enabling O(1) free-list operations
//! without any dynamic allocation.

pub mod page_meta;

use core::ptr;

pub use page_meta::{PFN_NONE, PageFlags, PageMeta};
use page_meta::{free_list_push, free_list_remove};

use crate::{
    align_up, eii,
    error::{AllocError, AllocResult},
    is_aligned,
};

/// Maximum buddy order. With 4 KiB pages this gives 2^20 × 4 KiB = 4 GiB blocks.
pub const MAX_ORDER: usize = 20;

/// DMA32 zone upper bound (4 GiB physical).
const DMA32_LIMIT: usize = 0x1_0000_0000;

fn normalize_region(
    region_start: usize,
    region_size: usize,
    granule: usize,
) -> Option<(usize, usize)> {
    if region_size == 0 || !granule.is_power_of_two() {
        return None;
    }
    let region_end = region_start.checked_add(region_size)?;
    let usable_start = align_up(region_start, granule);
    let usable_end = region_end & !(granule - 1);
    if usable_end <= usable_start {
        return None;
    }
    Some((usable_start, usable_end - usable_start))
}

pub(crate) struct RegionLayout {
    pub(crate) section_start: usize,
    pub(crate) meta_start: usize,
    pub(crate) managed_heap_start: usize,
    pub(crate) managed_heap_size: usize,
}

pub(crate) struct SectionInitSpec {
    pub(crate) region_start: usize,
    pub(crate) region_size: usize,
    pub(crate) section_ptr: *mut BuddySection,
    pub(crate) meta_ptr: *mut u8,
    pub(crate) meta_size: usize,
    pub(crate) heap_start: usize,
    pub(crate) heap_size: usize,
}

/// Public read-only summary of a managed section.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ManagedSection {
    pub start: usize,
    pub size: usize,
    pub free_pages: usize,
    pub total_pages: usize,
}

/// Per-region buddy state stored in the region prefix.
#[repr(C)]
pub(crate) struct BuddySection {
    pub(crate) next: *mut BuddySection,
    pub(crate) region_start: usize,
    pub(crate) region_size: usize,
    pub(crate) meta: *mut PageMeta,
    pub(crate) max_pages: usize,
    pub(crate) heap_start: usize,
    pub(crate) heap_size: usize,
    pub(crate) free_lists: [u32; MAX_ORDER + 1],
    pub(crate) free_pages: usize,
    pub(crate) total_pages: usize,
}

impl BuddySection {
    const fn metadata_align() -> usize {
        let section_align = core::mem::align_of::<BuddySection>();
        let meta_align = core::mem::align_of::<PageMeta>();
        if section_align > meta_align {
            section_align
        } else {
            meta_align
        }
    }

    fn metadata_layout_for_pages(pages: usize) -> Option<(usize, usize)> {
        let meta_offset = align_up(
            core::mem::size_of::<BuddySection>(),
            core::mem::align_of::<PageMeta>(),
        );
        let page_meta_size = pages.checked_mul(core::mem::size_of::<PageMeta>())?;
        let meta_size = meta_offset.checked_add(page_meta_size)?;
        Some((meta_offset, meta_size))
    }

    fn available_heap_pages<const PAGE_SIZE: usize>(
        region_end: usize,
        section_start: usize,
        meta_size: usize,
        heap_align: usize,
    ) -> Option<usize> {
        let managed_heap_start = align_up(section_start.checked_add(meta_size)?, heap_align);
        if managed_heap_start > region_end {
            return Some(0);
        }
        Some((region_end - managed_heap_start) / PAGE_SIZE)
    }

    fn can_manage_pages<const PAGE_SIZE: usize>(
        region_end: usize,
        section_start: usize,
        pages: usize,
        heap_align: usize,
    ) -> bool {
        let Some((_, meta_size)) = Self::metadata_layout_for_pages(pages) else {
            return false;
        };
        let Some(available_pages) = Self::available_heap_pages::<PAGE_SIZE>(
            region_end,
            section_start,
            meta_size,
            heap_align,
        ) else {
            return false;
        };
        available_pages >= pages
    }

    pub(crate) fn compute_region_layout_with_heap_align<const PAGE_SIZE: usize>(
        region_start: usize,
        region_size: usize,
        heap_align: usize,
    ) -> Option<RegionLayout> {
        if region_size == 0 || !PAGE_SIZE.is_power_of_two() || !heap_align.is_power_of_two() {
            return None;
        }

        let region_end = region_start.checked_add(region_size)?;
        let section_start = align_up(region_start, Self::metadata_align());
        if section_start >= region_end {
            return None;
        }

        let heap_search_start = align_up(
            section_start.checked_add(core::mem::size_of::<BuddySection>())?,
            PAGE_SIZE,
        );
        let max_pages = if heap_search_start >= region_end {
            0
        } else {
            (region_end - heap_search_start) / PAGE_SIZE
        };

        let mut low = 0usize;
        let mut high = max_pages;
        while low < high {
            let mid = low + (high - low).div_ceil(2);
            if Self::can_manage_pages::<PAGE_SIZE>(region_end, section_start, mid, heap_align) {
                low = mid;
            } else {
                high = mid - 1;
            }
        }

        if low == 0 {
            return None;
        }

        let (meta_offset, meta_size) = Self::metadata_layout_for_pages(low)?;
        let meta_start = section_start.checked_add(meta_offset)?;
        let managed_heap_start = align_up(section_start.checked_add(meta_size)?, heap_align);
        let managed_heap_size = low.checked_mul(PAGE_SIZE)?;

        Some(RegionLayout {
            section_start,
            meta_start,
            managed_heap_start,
            managed_heap_size,
        })
    }

    fn compute_region_layout<const PAGE_SIZE: usize>(
        region_start: usize,
        region_size: usize,
    ) -> Option<RegionLayout> {
        Self::compute_region_layout_with_heap_align::<PAGE_SIZE>(
            region_start,
            region_size,
            PAGE_SIZE,
        )
    }

    unsafe fn init_at<const PAGE_SIZE: usize>(
        section_ptr: *mut BuddySection,
        region_start: usize,
        region_size: usize,
        meta_ptr: *mut u8,
        meta_size: usize,
        heap_start: usize,
        heap_size: usize,
    ) -> AllocResult {
        unsafe {
            if !PAGE_SIZE.is_power_of_two() {
                return Err(AllocError::InvalidParam);
            }
            if !is_aligned(heap_start, PAGE_SIZE) || heap_size == 0 {
                return Err(AllocError::InvalidParam);
            }

            let total_pages = heap_size / PAGE_SIZE;
            let required = BuddyAllocator::<PAGE_SIZE>::required_meta_size(heap_size);
            if meta_size < required {
                return Err(AllocError::InvalidParam);
            }

            let meta = meta_ptr as *mut PageMeta;
            for i in 0..total_pages {
                meta.add(i).write(PageMeta::new());
            }

            section_ptr.write(BuddySection {
                next: ptr::null_mut(),
                region_start,
                region_size,
                meta,
                max_pages: total_pages,
                heap_start,
                heap_size,
                free_lists: [PFN_NONE; MAX_ORDER + 1],
                free_pages: 0,
                total_pages,
            });

            let section = &mut *section_ptr;
            let mut pfn: usize = 0;
            while pfn < total_pages {
                let mut order = MAX_ORDER;
                loop {
                    let block_pages = 1usize << order;
                    if block_pages <= total_pages - pfn && (pfn & (block_pages - 1)) == 0 {
                        break;
                    }
                    if order == 0 {
                        break;
                    }
                    order -= 1;
                }
                let block_pages = 1usize << order;
                let m = &mut *section.meta.add(pfn);
                m.flags = PageFlags::Free;
                m.order = order as u8;
                free_list_push(section.meta, &mut section.free_lists, pfn as u32, order);
                section.free_pages += block_pages;
                pfn += block_pages;
            }

            Ok(())
        }
    }

    #[inline]
    fn contains_heap_addr(&self, addr: usize) -> bool {
        addr >= self.heap_start && addr < self.heap_start + self.heap_size
    }

    #[inline]
    fn summary(&self) -> ManagedSection {
        ManagedSection {
            start: self.heap_start,
            size: self.heap_size,
            free_pages: self.free_pages,
            total_pages: self.total_pages,
        }
    }
}

/// Page-metadata-based buddy allocator.
///
/// `PAGE_SIZE` must be a power of two (commonly 0x1000 = 4 KiB).
pub struct BuddyAllocator<const PAGE_SIZE: usize = 0x1000> {
    sections_head: *mut BuddySection,
    sections_tail: *mut BuddySection,
    section_count: usize,
}

// SAFETY: The allocator is designed to be wrapped in a SpinMutex.
// All section pointers point into caller-provided regions whose lifetime is managed externally.
unsafe impl<const PAGE_SIZE: usize> Send for BuddyAllocator<PAGE_SIZE> {}

impl<const PAGE_SIZE: usize> BuddyAllocator<PAGE_SIZE> {
    /// Calculate the metadata-region size (in bytes) required for `heap_size` bytes.
    pub const fn required_meta_size(heap_size: usize) -> usize {
        let pages = heap_size / PAGE_SIZE;
        pages * core::mem::size_of::<PageMeta>()
    }

    /// Create an uninitialised allocator. Call [`init`](Self::init) before use.
    pub const fn new() -> Self {
        Self {
            sections_head: ptr::null_mut(),
            sections_tail: ptr::null_mut(),
            section_count: 0,
        }
    }
}

impl<const PAGE_SIZE: usize> Default for BuddyAllocator<PAGE_SIZE> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const PAGE_SIZE: usize> BuddyAllocator<PAGE_SIZE> {
    pub(crate) fn reset(&mut self) {
        self.sections_head = ptr::null_mut();
        self.sections_tail = ptr::null_mut();
        self.section_count = 0;
    }

    /// Initialise the allocator over the first section.
    ///
    /// # Safety
    /// - `region` must be writable and remain valid for the lifetime of this allocator.
    /// - Bytes consumed by metadata become unavailable for allocation.
    pub unsafe fn init(&mut self, region: &mut [u8]) -> AllocResult {
        unsafe {
            self.reset();
            self.add_region(region)
        }
    }

    /// Add a new managed region after initialisation.
    ///
    /// # Safety
    /// - `region` must be writable and remain valid for the lifetime of this allocator.
    /// - The region must not overlap any existing managed region.
    pub unsafe fn add_region(&mut self, region: &mut [u8]) -> AllocResult {
        unsafe {
            let region_start = region.as_mut_ptr() as usize;
            let region_size = region.len();
            let (region_start, region_size) =
                normalize_region(region_start, region_size, PAGE_SIZE)
                    .ok_or(AllocError::InvalidParam)?;
            let layout =
                BuddySection::compute_region_layout::<PAGE_SIZE>(region_start, region_size)
                    .ok_or(AllocError::InvalidParam)?;
            self.add_region_raw(SectionInitSpec {
                region_start,
                region_size,
                section_ptr: layout.section_start as *mut BuddySection,
                meta_ptr: layout.meta_start as *mut u8,
                meta_size: Self::required_meta_size(layout.managed_heap_size),
                heap_start: layout.managed_heap_start,
                heap_size: layout.managed_heap_size,
            })
        }
    }

    pub(crate) unsafe fn add_region_raw(&mut self, spec: SectionInitSpec) -> AllocResult {
        unsafe {
            let region_size = spec.region_size;
            let region_end = spec
                .region_start
                .checked_add(region_size)
                .ok_or(AllocError::InvalidParam)?;
            let heap_end = spec
                .heap_start
                .checked_add(spec.heap_size)
                .ok_or(AllocError::InvalidParam)?;
            if heap_end > region_end {
                return Err(AllocError::InvalidParam);
            }

            let mut section = self.sections_head;
            while !section.is_null() {
                let existing = &*section;
                let existing_end = existing
                    .region_start
                    .checked_add(existing.region_size)
                    .ok_or(AllocError::InvalidParam)?;
                if spec.region_start < existing_end && existing.region_start < region_end {
                    return Err(AllocError::MemoryOverlap);
                }
                section = existing.next;
            }

            BuddySection::init_at::<PAGE_SIZE>(
                spec.section_ptr,
                spec.region_start,
                spec.region_size,
                spec.meta_ptr,
                spec.meta_size,
                spec.heap_start,
                spec.heap_size,
            )?;

            if self.sections_head.is_null() {
                self.sections_head = spec.section_ptr;
            } else {
                (*self.sections_tail).next = spec.section_ptr;
            }
            self.sections_tail = spec.section_ptr;
            self.section_count += 1;

            log::debug!(
                "BuddyAllocator: add section region {:#x}+{:#x}, heap {:#x}..{:#x}, {} pages",
                spec.region_start,
                spec.region_size,
                spec.heap_start,
                heap_end,
                spec.heap_size / PAGE_SIZE,
            );

            Ok(())
        }
    }

    /// Number of managed sections.
    pub fn section_count(&self) -> usize {
        self.section_count
    }

    /// Read-only summary for a managed section by registration order.
    pub fn section(&self, index: usize) -> Option<ManagedSection> {
        let mut current = self.sections_head;
        let mut i = 0usize;
        while !current.is_null() {
            if i == index {
                return Some(unsafe { (&*current).summary() });
            }
            current = unsafe { (*current).next };
            i += 1;
        }
        None
    }

    /// Total number of pages managed across all sections.
    pub fn total_pages(&self) -> usize {
        let mut total = 0usize;
        let mut current = self.sections_head;
        while !current.is_null() {
            total += unsafe { (*current).total_pages };
            current = unsafe { (*current).next };
        }
        total
    }

    /// Total managed heap bytes across all sections.
    ///
    /// This counts only bytes in allocatable heaps, excluding region-prefix metadata.
    pub fn managed_bytes(&self) -> usize {
        let mut total = 0usize;
        let mut current = self.sections_head;
        while !current.is_null() {
            total += unsafe { (*current).heap_size };
            current = unsafe { (*current).next };
        }
        total
    }

    /// Number of currently free pages across all sections.
    pub fn free_pages(&self) -> usize {
        let mut total = 0usize;
        let mut current = self.sections_head;
        while !current.is_null() {
            total += unsafe { (*current).free_pages };
            current = unsafe { (*current).next };
        }
        total
    }

    /// Allocated backend bytes across all sections.
    ///
    /// This is computed as managed heap bytes minus currently free page bytes.
    /// It reflects page-level occupancy, so it includes slab pages, alignment
    /// amplification, and internal fragmentation.
    pub fn allocated_bytes(&self) -> usize {
        self.managed_bytes()
            .saturating_sub(self.free_pages().saturating_mul(PAGE_SIZE))
    }

    /// Allocate `count` contiguous pages, returning the virtual address.
    pub fn alloc_pages(&mut self, count: usize, align: usize) -> AllocResult<usize> {
        if count == 0 {
            return Err(AllocError::InvalidParam);
        }
        let align = if align == 0 { PAGE_SIZE } else { align };
        if !align.is_power_of_two() || align < PAGE_SIZE {
            return Err(AllocError::InvalidParam);
        }

        let order = count.next_power_of_two().trailing_zeros() as usize;
        if order > MAX_ORDER {
            return Err(AllocError::InvalidParam);
        }

        let mut section = self.sections_head;
        while !section.is_null() {
            if let Ok(addr) =
                unsafe { Self::alloc_from_section_aligned(&mut *section, order, align) }
            {
                return Ok(addr);
            }
            section = unsafe { (*section).next };
        }

        Err(AllocError::NoMemory)
    }

    fn alloc_from_section_aligned(
        section: &mut BuddySection,
        order: usize,
        align: usize,
    ) -> AllocResult<usize> {
        for search_order in order..=MAX_ORDER {
            let mut pfn_u32 = section.free_lists[search_order];
            while pfn_u32 != PFN_NONE {
                let block_pfn = pfn_u32 as usize;
                if let Some(target_pfn) = Self::find_aligned_pfn_in_block(
                    section.heap_start,
                    block_pfn,
                    search_order,
                    order,
                    align,
                ) {
                    unsafe {
                        free_list_remove(
                            section.meta,
                            &mut section.free_lists,
                            pfn_u32,
                            search_order,
                        );
                    }

                    let mut current_order = search_order;
                    let mut current_pfn = block_pfn;
                    while current_order > order {
                        current_order -= 1;
                        let left_pfn = current_pfn;
                        let right_pfn = current_pfn + (1 << current_order);
                        let (next_pfn, free_pfn) = if target_pfn >= right_pfn {
                            (right_pfn, left_pfn)
                        } else {
                            (left_pfn, right_pfn)
                        };
                        unsafe {
                            let bm = &mut *section.meta.add(free_pfn);
                            bm.flags = PageFlags::Free;
                            bm.order = current_order as u8;
                            free_list_push(
                                section.meta,
                                &mut section.free_lists,
                                free_pfn as u32,
                                current_order,
                            );
                        }
                        current_pfn = next_pfn;
                    }

                    unsafe {
                        let m = &mut *section.meta.add(current_pfn);
                        m.flags = PageFlags::Allocated;
                        m.order = order as u8;
                    }

                    section.free_pages -= 1 << order;
                    return Ok(section.heap_start + current_pfn * PAGE_SIZE);
                }
                pfn_u32 = unsafe { (*section.meta.add(pfn_u32 as usize)).next };
            }
        }

        Err(AllocError::NoMemory)
    }

    fn find_aligned_pfn_in_block(
        heap_start: usize,
        block_pfn: usize,
        block_order: usize,
        alloc_order: usize,
        align: usize,
    ) -> Option<usize> {
        let subblock_pages = 1usize << alloc_order;
        let align_pages = align / PAGE_SIZE;
        let heap_page_offset = (heap_start / PAGE_SIZE) & (align_pages - 1);
        let offset = (align_pages - heap_page_offset) & (align_pages - 1);

        let candidate = if align_pages <= subblock_pages {
            if !heap_start.is_multiple_of(align) {
                return None;
            }
            block_pfn
        } else {
            if !offset.is_multiple_of(subblock_pages) {
                return None;
            }
            let rem = block_pfn & (align_pages - 1);
            let delta = (offset + align_pages - rem) & (align_pages - 1);
            block_pfn + delta
        };

        let block_pages = 1usize << block_order;
        let last_start = block_pfn + block_pages - subblock_pages;
        (candidate <= last_start).then_some(candidate)
    }

    /// Allocate pages whose *physical* address is below 4 GiB (DMA32 zone).
    pub fn alloc_pages_lowmem(&mut self, count: usize, align: usize) -> AllocResult<usize> {
        if count == 0 {
            return Err(AllocError::InvalidParam);
        }
        let align = if align == 0 { PAGE_SIZE } else { align };
        if !align.is_power_of_two() || align < PAGE_SIZE {
            return Err(AllocError::InvalidParam);
        }

        let order = count.next_power_of_two().trailing_zeros() as usize;
        if order > MAX_ORDER {
            return Err(AllocError::InvalidParam);
        }

        let mut section = self.sections_head;
        while !section.is_null() {
            if let Ok(addr) =
                unsafe { Self::alloc_lowmem_from_section(&mut *section, order, align) }
            {
                return Ok(addr);
            }
            section = unsafe { (*section).next };
        }

        Err(AllocError::NoMemory)
    }

    fn alloc_lowmem_from_section(
        section: &mut BuddySection,
        alloc_order: usize,
        align: usize,
    ) -> AllocResult<usize> {
        for search_order in alloc_order..=MAX_ORDER {
            let mut pfn_u32 = section.free_lists[search_order];
            while pfn_u32 != PFN_NONE {
                let block_pfn = pfn_u32 as usize;
                let Some(target_pfn) = Self::find_aligned_pfn_in_block(
                    section.heap_start,
                    block_pfn,
                    search_order,
                    alloc_order,
                    align,
                ) else {
                    pfn_u32 = unsafe { (*section.meta.add(pfn_u32 as usize)).next };
                    continue;
                };
                let addr = section.heap_start + target_pfn * PAGE_SIZE;
                let phys = eii::virt_to_phys(addr);
                let block_bytes = (1usize << alloc_order) * PAGE_SIZE;
                if phys + block_bytes <= DMA32_LIMIT && addr.is_multiple_of(align) {
                    unsafe {
                        free_list_remove(
                            section.meta,
                            &mut section.free_lists,
                            pfn_u32,
                            search_order,
                        );
                    }

                    let mut current_order = search_order;
                    let mut current_pfn = block_pfn;
                    while current_order > alloc_order {
                        current_order -= 1;
                        let left_pfn = current_pfn;
                        let right_pfn = current_pfn + (1 << current_order);
                        let (next_pfn, free_pfn) = if target_pfn >= right_pfn {
                            (right_pfn, left_pfn)
                        } else {
                            (left_pfn, right_pfn)
                        };
                        unsafe {
                            let bm = &mut *section.meta.add(free_pfn);
                            bm.flags = PageFlags::Free;
                            bm.order = current_order as u8;
                            free_list_push(
                                section.meta,
                                &mut section.free_lists,
                                free_pfn as u32,
                                current_order,
                            );
                        }
                        current_pfn = next_pfn;
                    }

                    unsafe {
                        let m = &mut *section.meta.add(current_pfn);
                        m.flags = PageFlags::Allocated;
                        m.order = alloc_order as u8;
                    }
                    section.free_pages -= 1 << alloc_order;
                    return Ok(addr);
                }
                pfn_u32 = unsafe { (*section.meta.add(pfn_u32 as usize)).next };
            }
        }

        Err(AllocError::NoMemory)
    }

    /// Free pages previously obtained via [`alloc_pages`](Self::alloc_pages).
    ///
    /// `addr` must be the exact address returned by alloc. The allocator frees
    /// the full block size recorded in page metadata, which may be larger than
    /// `count` if the original allocation was rounded up for buddy order or alignment.
    pub fn dealloc_pages(&mut self, addr: usize, count: usize) {
        let Some(section) = self.find_section_by_addr_mut(addr) else {
            debug_assert!(
                false,
                "dealloc_pages called with address outside all sections"
            );
            return;
        };

        debug_assert!(is_aligned(addr, PAGE_SIZE));
        debug_assert!(count > 0);

        let pfn = (addr - section.heap_start) / PAGE_SIZE;
        debug_assert!(pfn < section.max_pages);
        let stored = unsafe { &*section.meta.add(pfn) };
        debug_assert!(
            stored.flags == PageFlags::Allocated || stored.flags == PageFlags::Slab,
            "dealloc_pages called on non-allocated block"
        );

        let expected_order = count.next_power_of_two().trailing_zeros() as usize;
        let order = stored.order as usize;
        debug_assert!(
            expected_order <= order,
            "dealloc_pages count implies larger order than the allocated block"
        );
        Self::dealloc_in_section(section, pfn, order);
    }

    /// Mark the page at `addr` with the given flags (used by slab to tag pages).
    ///
    /// # Safety
    /// The caller must ensure `addr` is valid and properly allocated.
    pub unsafe fn set_page_flags(&mut self, addr: usize, flags: PageFlags) -> AllocResult {
        unsafe {
            let section = self
                .find_section_by_addr_mut(addr)
                .ok_or(AllocError::NotFound)?;
            let pfn = (addr - section.heap_start) / PAGE_SIZE;
            (*section.meta.add(pfn)).flags = flags;
            Ok(())
        }
    }

    /// Read the flags of the page containing `addr`.
    pub fn page_flags(&self, addr: usize) -> AllocResult<PageFlags> {
        let section = self
            .find_section_by_addr(addr)
            .ok_or(AllocError::NotFound)?;
        let pfn = (addr - section.heap_start) / PAGE_SIZE;
        Ok(unsafe { (*section.meta.add(pfn)).flags })
    }

    fn dealloc_in_section(section: &mut BuddySection, mut pfn: usize, mut order: usize) {
        let freed_pages = 1usize << order;

        while order < MAX_ORDER {
            let buddy_pfn = pfn ^ (1 << order);
            if buddy_pfn >= section.max_pages {
                break;
            }
            let buddy = unsafe { &*section.meta.add(buddy_pfn) };
            if buddy.flags != PageFlags::Free || buddy.order as usize != order {
                break;
            }
            unsafe {
                free_list_remove(
                    section.meta,
                    &mut section.free_lists,
                    buddy_pfn as u32,
                    order,
                );
            }
            pfn = pfn.min(buddy_pfn);
            order += 1;
        }

        unsafe {
            let m = &mut *section.meta.add(pfn);
            m.flags = PageFlags::Free;
            m.order = order as u8;
            free_list_push(section.meta, &mut section.free_lists, pfn as u32, order);
        }
        section.free_pages += freed_pages;
    }

    fn find_section_by_addr(&self, addr: usize) -> Option<&BuddySection> {
        let mut section = self.sections_head;
        while !section.is_null() {
            let current = unsafe { &*section };
            if current.contains_heap_addr(addr) {
                return Some(current);
            }
            section = current.next;
        }
        None
    }

    fn find_section_by_addr_mut(&mut self, addr: usize) -> Option<&mut BuddySection> {
        let mut section = self.sections_head;
        while !section.is_null() {
            let current = unsafe { &mut *section };
            if current.contains_heap_addr(addr) {
                return Some(current);
            }
            section = current.next;
        }
        None
    }
}
