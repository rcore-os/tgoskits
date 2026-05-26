/// Global allocator composing buddy (pages) + per-CPU slab (objects).
///
/// Implements [`core::alloc::GlobalAlloc`] so it can serve as `#[global_allocator]`.
/// Cross-CPU frees are lock-free via [`SlabPageHeader::remote_free`].
use core::alloc::{GlobalAlloc, Layout};
use core::{
    ptr::{self, NonNull},
    sync::atomic::{AtomicBool, Ordering},
};

use spin::Mutex as SpinMutex;

use crate::{
    align_up,
    buddy::{BuddyAllocator, BuddySection, ManagedSection, PageFlags, SectionInitSpec},
    eii,
    error::{AllocError, AllocResult},
    slab::{
        SlabAllocResult,
        page::{SLAB_MAGIC, SlabPageHeader},
        size_class::{SLAB_MAX_SIZE, SizeClass},
    },
};

const REGION_GRANULE: usize = 2 * 1024 * 1024;
static GLOBAL_ALLOCATOR_LIVE: AtomicBool = AtomicBool::new(false);

#[doc(hidden)]
pub fn __reset_global_allocator_singleton_for_tests() {
    GLOBAL_ALLOCATOR_LIVE.store(false, Ordering::Release);
}

/// Unified allocator: buddy page allocator + per-CPU slab caches.
pub struct GlobalAllocator<const PAGE_SIZE: usize = 0x1000> {
    buddy: SpinMutex<BuddyAllocator<PAGE_SIZE>>,
    initialized: AtomicBool,
}

// SAFETY: All mutable state is behind SpinMutex or AtomicBool.
unsafe impl<const PAGE_SIZE: usize> Sync for GlobalAllocator<PAGE_SIZE> {}
unsafe impl<const PAGE_SIZE: usize> Send for GlobalAllocator<PAGE_SIZE> {}

impl<const PAGE_SIZE: usize> GlobalAllocator<PAGE_SIZE> {
    /// Create an uninitialised global allocator.
    pub const fn new() -> Self {
        Self {
            buddy: SpinMutex::new(BuddyAllocator::new()),
            initialized: AtomicBool::new(false),
        }
    }
}

impl<const PAGE_SIZE: usize> Default for GlobalAllocator<PAGE_SIZE> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const PAGE_SIZE: usize> GlobalAllocator<PAGE_SIZE> {
    /// Initialise the allocator over the first region.
    ///
    /// # Safety
    /// - `region` must be writable and remain valid for the lifetime of this allocator.
    /// - Any bytes consumed by metadata or alignment padding become unavailable for allocation.
    pub unsafe fn init(&self, region: &mut [u8]) -> AllocResult {
        unsafe {
            if self.initialized.load(Ordering::Acquire) {
                return Err(AllocError::AlreadyInitialized);
            }
            if GLOBAL_ALLOCATOR_LIVE
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
            {
                return Err(AllocError::AlreadyInitialized);
            }

            let raw_region_start = region.as_mut_ptr() as usize;
            let raw_region_size = region.len();
            let layout = match BuddySection::compute_region_layout_with_heap_align::<PAGE_SIZE>(
                raw_region_start,
                raw_region_size,
                REGION_GRANULE,
            )
            .ok_or(AllocError::InvalidParam)
            {
                Ok(layout) => layout,
                Err(err) => {
                    GLOBAL_ALLOCATOR_LIVE.store(false, Ordering::Release);
                    return Err(err);
                }
            };

            let mut buddy = self.buddy.lock();
            buddy.reset();
            if let Err(err) = buddy.add_region_raw(SectionInitSpec {
                region_start: raw_region_start,
                region_size: raw_region_size,
                section_ptr: layout.section_start as *mut BuddySection,
                meta_ptr: layout.meta_start as *mut u8,
                meta_size: BuddyAllocator::<PAGE_SIZE>::required_meta_size(
                    layout.managed_heap_size,
                ),
                heap_start: layout.managed_heap_start,
                heap_size: layout.managed_heap_size,
            }) {
                GLOBAL_ALLOCATOR_LIVE.store(false, Ordering::Release);
                return Err(err);
            }
            drop(buddy);

            self.initialized.store(true, Ordering::Release);

            log::debug!(
                "GlobalAllocator: region {:#x}+{:#x}, section {:#x}, first heap {:#x}+{:#x}",
                raw_region_start,
                raw_region_size,
                layout.section_start,
                layout.managed_heap_start,
                layout.managed_heap_size,
            );

            Ok(())
        }
    }

    /// Add a new managed region after [`init`](Self::init).
    ///
    /// # Safety
    /// - `region` must be writable and remain valid for the lifetime of this allocator.
    /// - The region must not overlap any already managed region.
    pub unsafe fn add_region(&self, region: &mut [u8]) -> AllocResult {
        unsafe {
            if !self.initialized.load(Ordering::Acquire) {
                return Err(AllocError::NotInitialized);
            }
            let region_start = region.as_mut_ptr() as usize;
            let region_size = region.len();
            let Some(layout) = BuddySection::compute_region_layout_with_heap_align::<PAGE_SIZE>(
                region_start,
                region_size,
                REGION_GRANULE,
            ) else {
                log::info!(
                    "GlobalAllocator: skip region {:#x}+{:#x}, no allocator-visible memory after \
                     {} alignment",
                    region_start,
                    region_size,
                    REGION_GRANULE,
                );
                return Ok(());
            };
            self.buddy.lock().add_region_raw(SectionInitSpec {
                region_start,
                region_size,
                section_ptr: layout.section_start as *mut BuddySection,
                meta_ptr: layout.meta_start as *mut u8,
                meta_size: BuddyAllocator::<PAGE_SIZE>::required_meta_size(
                    layout.managed_heap_size,
                ),
                heap_start: layout.managed_heap_start,
                heap_size: layout.managed_heap_size,
            })
        }
    }

    /// Number of managed sections.
    pub fn managed_section_count(&self) -> usize {
        self.buddy.lock().section_count()
    }

    /// Read-only summary for a managed section.
    pub fn managed_section(&self, index: usize) -> Option<ManagedSection> {
        self.buddy.lock().section(index)
    }

    /// Total managed heap bytes across all sections.
    ///
    /// This excludes region-prefix metadata such as `BuddySection` and `PageMeta[]`.
    pub fn managed_bytes(&self) -> usize {
        self.buddy.lock().managed_bytes()
    }

    /// Allocated backend bytes across all sections.
    ///
    /// This is page-level occupancy, not the exact sum of requested layout sizes.
    pub fn allocated_bytes(&self) -> usize {
        self.buddy.lock().allocated_bytes()
    }

    /// Allocate contiguous pages. Returns the virtual start address.
    pub fn alloc_pages(&self, count: usize, align: usize) -> AllocResult<usize> {
        self.buddy.lock().alloc_pages(count, align)
    }

    /// Free pages previously obtained via [`alloc_pages`](Self::alloc_pages).
    pub fn dealloc_pages(&self, addr: usize, count: usize) {
        self.buddy.lock().dealloc_pages(addr, count);
    }

    /// Allocate pages with physical address below 4 GiB.
    pub fn alloc_pages_lowmem(&self, count: usize, align: usize) -> AllocResult<usize> {
        self.buddy.lock().alloc_pages_lowmem(count, align)
    }

    /// Allocate memory for `layout`. Returns a pointer on success.
    pub fn alloc(&self, layout: Layout) -> AllocResult<NonNull<u8>> {
        if !self.initialized.load(Ordering::Acquire) {
            return Err(AllocError::NotInitialized);
        }

        if self.is_slab_eligible(&layout) {
            self.slab_alloc(layout)
        } else {
            self.large_alloc(layout)
        }
    }

    /// Deallocate memory previously returned by [`alloc`](Self::alloc).
    ///
    /// # Safety
    /// `ptr` must have been returned by a prior `alloc` with the same `layout`.
    pub unsafe fn dealloc(&self, ptr: NonNull<u8>, layout: Layout) {
        unsafe {
            if self.is_slab_eligible(&layout) {
                self.slab_dealloc(ptr, layout);
            } else {
                self.large_dealloc(ptr, layout);
            }
        }
    }

    #[inline]
    fn is_slab_eligible(&self, layout: &Layout) -> bool {
        layout.size() <= SLAB_MAX_SIZE && layout.align() <= SLAB_MAX_SIZE
    }

    fn slab_alloc(&self, layout: Layout) -> AllocResult<NonNull<u8>> {
        let pool = eii::slab_pool();

        match pool.alloc(layout)? {
            SlabAllocResult::Allocated(ptr) => Ok(ptr),
            SlabAllocResult::NeedsSlab { size_class, pages } => {
                let bytes = pages * PAGE_SIZE;
                let addr = self.buddy.lock().alloc_pages(pages, bytes)?;
                unsafe {
                    self.buddy.lock().set_page_flags(addr, PageFlags::Slab)?;
                }
                pool.add_slab(size_class, addr, bytes);
                match pool.alloc(layout)? {
                    SlabAllocResult::Allocated(ptr) => Ok(ptr),
                    SlabAllocResult::NeedsSlab { .. } => Err(AllocError::NoMemory),
                }
            }
        }
    }

    unsafe fn slab_dealloc(&self, ptr: NonNull<u8>, layout: Layout) {
        unsafe {
            let sc = SizeClass::from_layout(layout).expect("layout exceeds slab");
            let slab_bytes = sc.slab_pages(PAGE_SIZE) * PAGE_SIZE;
            let base =
                SlabPageHeader::base_from_obj_addr::<PAGE_SIZE>(ptr.as_ptr() as usize, slab_bytes);
            let hdr = &*(base as *const SlabPageHeader);
            debug_assert_eq!(hdr.magic, SLAB_MAGIC);

            match eii::slab_pool().dealloc(ptr, layout, hdr.owner_cpu as usize) {
                crate::SlabPoolDeallocResult::Done => {}
                crate::SlabPoolDeallocResult::RemoteQueued => {}
                crate::SlabPoolDeallocResult::FreeSlab { base, pages } => {
                    self.buddy.lock().dealloc_pages(base, pages);
                }
            }
        }
    }

    fn large_alloc(&self, layout: Layout) -> AllocResult<NonNull<u8>> {
        let pages = align_up(layout.size(), PAGE_SIZE) / PAGE_SIZE;
        let align = layout.align().max(PAGE_SIZE);
        let addr = self.buddy.lock().alloc_pages(pages, align)?;
        Ok(unsafe { NonNull::new_unchecked(addr as *mut u8) })
    }

    unsafe fn large_dealloc(&self, ptr: NonNull<u8>, layout: Layout) {
        let pages = align_up(layout.size(), PAGE_SIZE) / PAGE_SIZE;
        self.buddy
            .lock()
            .dealloc_pages(ptr.as_ptr() as usize, pages);
    }
}

impl<const PAGE_SIZE: usize> Drop for GlobalAllocator<PAGE_SIZE> {
    fn drop(&mut self) {
        if self.initialized.swap(false, Ordering::AcqRel) {
            GLOBAL_ALLOCATOR_LIVE.store(false, Ordering::Release);
        }
    }
}

unsafe impl<const PAGE_SIZE: usize> GlobalAlloc for GlobalAllocator<PAGE_SIZE> {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        match self.alloc(layout) {
            Ok(ptr) => ptr.as_ptr(),
            Err(_) => ptr::null_mut(),
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe {
            if let Some(nn) = NonNull::new(ptr) {
                self.dealloc(nn, layout);
            }
        }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        unsafe {
            let new_layout = match Layout::from_size_align(new_size, layout.align()) {
                Ok(l) => l,
                Err(_) => return ptr::null_mut(),
            };

            let new_ptr = <Self as GlobalAlloc>::alloc(self, new_layout);
            if !new_ptr.is_null() {
                let copy_size = layout.size().min(new_size);
                ptr::copy_nonoverlapping(ptr, new_ptr, copy_size);
                <Self as GlobalAlloc>::dealloc(self, ptr, layout);
            }
            new_ptr
        }
    }
}
