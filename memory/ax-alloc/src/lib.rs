//! [ArceOS](https://github.com/arceos-org/arceos) global memory allocator.
//!
//! It provides [`GlobalAllocator`], which implements the trait
//! [`core::alloc::GlobalAlloc`]. A static global variable of type
//! [`GlobalAllocator`] is defined with the `#[global_allocator]` attribute, to
//! be registered as the standard library's default allocator.

#![no_std]

#[allow(unused_imports)]
#[macro_use]
extern crate log;
extern crate alloc;

use core::{
    alloc::Layout,
    fmt,
    ptr::NonNull,
    sync::atomic::{AtomicBool, Ordering},
};

use strum::{IntoStaticStr, VariantArray};

const PAGE_SIZE: usize = 0x1000;
#[cfg(any(tlsf, buddy_slab, test))]
const MIN_RECLAIM_PAGES: usize = 16;
#[cfg(any(tlsf, buddy_slab, test))]
const MAX_RECLAIM_ATTEMPTS: usize = 4;

/// A function that tries to reclaim physical pages (e.g. by evicting
/// clean file-backed page cache pages). Returns the number of pages freed.
pub type PageReclaimFn = fn(num_pages: usize) -> usize;

static PAGE_RECLAIM_FN: ax_sync::SpinLock<Option<PageReclaimFn>> = ax_sync::SpinLock::new(None);
static PAGE_RECLAIM_ACTIVE: AtomicBool = AtomicBool::new(false);

struct PageReclaimLease;

impl PageReclaimLease {
    fn try_acquire() -> Option<Self> {
        PAGE_RECLAIM_ACTIVE
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| Self)
    }
}

impl Drop for PageReclaimLease {
    fn drop(&mut self) {
        PAGE_RECLAIM_ACTIVE.store(false, Ordering::Release);
    }
}

/// Register a callback that the allocator invokes when a page or Rust heap
/// allocation cannot be satisfied.
///
/// The callback is an allocator-pressure capability: it must not allocate from
/// this allocator, perform I/O, wait for contended locks, or invoke unknown
/// callbacks. It may use try-lock based, clean-page-only eviction.
pub fn register_page_reclaim_fn(f: PageReclaimFn) {
    *PAGE_RECLAIM_FN.lock_irqsave() = Some(f);
}

/// Try to reclaim physical pages by invoking the registered callback.
/// Returns the number of pages actually freed.
///
/// The registration lock and allocator backend lock are released before the
/// callback runs. A typed lease rejects recursive or concurrent reclaim; this
/// is the allocator equivalent of Linux's bounded direct-reclaim context.
pub fn try_page_reclaim(num_pages: usize) -> usize {
    let Some(_lease) = PageReclaimLease::try_acquire() else {
        return 0;
    };
    let reclaim_fn = { *PAGE_RECLAIM_FN.lock_irqsave() };
    reclaim_fn.map_or(0, |f| f(num_pages))
}

#[cfg(any(tlsf, buddy_slab, test))]
pub(crate) fn retry_after_page_reclaim<T>(
    target_pages: usize,
    mut attempt: impl FnMut() -> AllocResult<T>,
    mut reclaim: impl FnMut(usize) -> usize,
) -> AllocResult<T> {
    match attempt() {
        Ok(value) => return Ok(value),
        Err(AllocError::NoMemory) => {}
        Err(error) => return Err(error),
    }

    let target_pages = target_pages.max(MIN_RECLAIM_PAGES);
    for _ in 0..MAX_RECLAIM_ATTEMPTS {
        let reclaimed = reclaim(target_pages);

        // Retry even without local progress: another CPU may have completed a
        // reclaim or deallocation after the first allocation attempt failed.
        match attempt() {
            Ok(value) => return Ok(value),
            Err(AllocError::NoMemory) if reclaimed != 0 => {}
            Err(error) => return Err(error),
        }
    }
    Err(AllocError::NoMemory)
}

#[cfg(any(tlsf, buddy_slab))]
pub(crate) fn retry_after_registered_reclaim<T>(
    target_pages: usize,
    attempt: impl FnMut() -> AllocResult<T>,
) -> AllocResult<T> {
    retry_after_page_reclaim(target_pages, attempt, try_page_reclaim)
}

#[cfg(any(tlsf, buddy_slab))]
pub(crate) const fn layout_reclaim_pages(layout: Layout) -> usize {
    layout.size().div_ceil(PAGE_SIZE)
}

mod page;
pub use page::GlobalPage;

/// Tracking of memory usage, enabled with the `tracking` feature.
#[cfg(feature = "tracking")]
pub mod tracking;

/// Kinds of memory usage for tracking.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, VariantArray, IntoStaticStr)]
pub enum UsageKind {
    /// Heap allocations made by kernel Rust code.
    RustHeap,
    /// Virtual memory, usually used for user space.
    VirtMem,
    /// Page cache for file systems.
    PageCache,
    /// Page tables.
    PageTable,
    /// Page-backed kernel task stacks.
    TaskStack,
    /// DMA memory.
    Dma,
    /// Memory used by [`GlobalPage`].
    Global,
}

/// Statistics of memory usages.
#[derive(Clone, Copy)]
pub struct Usages([usize; UsageKind::VARIANTS.len()]);

impl Usages {
    const fn new() -> Self {
        Self([0; UsageKind::VARIANTS.len()])
    }

    #[allow(dead_code)]
    fn alloc(&mut self, kind: UsageKind, size: usize) {
        self.0[kind as usize] += size;
    }

    #[allow(dead_code)]
    fn dealloc(&mut self, kind: UsageKind, size: usize) {
        self.0[kind as usize] -= size;
    }

    /// Get the memory usage for a specific kind.
    pub fn get(&self, kind: UsageKind) -> usize {
        self.0[kind as usize]
    }
}

impl fmt::Debug for Usages {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut d = f.debug_struct("UsageStats");
        for &kind in UsageKind::VARIANTS {
            d.field(kind.into(), &self.0[kind as usize]);
        }
        d.finish()
    }
}

/// The error type used for allocation operations in `ax-alloc`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AllocError {
    /// Invalid size, alignment, or other input parameter.
    #[error("invalid allocation parameter")]
    InvalidParam,
    /// The allocator has already been initialized.
    #[error("allocator is already initialized")]
    AlreadyInitialized,
    /// A region overlaps with an existing managed region.
    #[error("memory region overlaps an existing allocation region")]
    MemoryOverlap,
    /// Not enough memory is available to satisfy the request.
    #[error("not enough memory")]
    NoMemory,
    /// Attempted to deallocate memory that was not allocated.
    #[error("memory was not allocated by this allocator")]
    NotAllocated,
    /// The allocator has not been initialized.
    #[error("allocator is not initialized")]
    NotInitialized,
    /// The requested address or entity was not found.
    #[error("allocation was not found")]
    NotFound,
}

/// A [`Result`] alias with [`AllocError`] as the error type.
pub type AllocResult<T = ()> = Result<T, AllocError>;

/// Unified allocator operations provided by all `ax-alloc` backends.
pub trait AllocatorOps {
    /// Returns the allocator name.
    fn name(&self) -> &'static str;

    /// Initializes the allocator with the given region.
    fn init(&self, start_vaddr: usize, size: usize) -> AllocResult;

    /// Adds an extra memory region to the allocator.
    fn add_memory(&self, start_vaddr: usize, size: usize) -> AllocResult;

    /// Allocates arbitrary bytes.
    fn alloc(&self, layout: Layout) -> AllocResult<NonNull<u8>>;

    /// Deallocates a prior byte allocation.
    fn dealloc(&self, pos: NonNull<u8>, layout: Layout);

    /// Allocates contiguous pages.
    ///
    /// `align` is the requested byte alignment, not a log2/exponent.
    /// It must be a power-of-two byte alignment accepted by the backend page allocator.
    fn alloc_pages(&self, num_pages: usize, align: usize, kind: UsageKind) -> AllocResult<usize>;

    /// Allocates contiguous DMA32 pages.
    ///
    /// `align` is the requested byte alignment, not a log2/exponent.
    /// It must be a power-of-two byte alignment accepted by the backend page allocator.
    fn alloc_dma32_pages(
        &self,
        num_pages: usize,
        align: usize,
        kind: UsageKind,
    ) -> AllocResult<usize>;

    /// Allocates contiguous pages starting from the given address.
    ///
    /// `align` is the requested byte alignment, not a log2/exponent.
    /// It must be a power-of-two byte alignment accepted by the backend page allocator.
    fn alloc_pages_at(
        &self,
        start: usize,
        num_pages: usize,
        align: usize,
        kind: UsageKind,
    ) -> AllocResult<usize>;

    /// Deallocates a prior page allocation.
    fn dealloc_pages(&self, pos: usize, num_pages: usize, kind: UsageKind);

    /// Returns used byte count.
    fn used_bytes(&self) -> usize;

    /// Returns available byte count.
    fn available_bytes(&self) -> usize;

    /// Returns used page count.
    fn used_pages(&self) -> usize;

    /// Returns available page count.
    fn available_pages(&self) -> usize;

    /// Returns usage statistics.
    fn usages(&self) -> Usages;
}

// Select implementation based on build.rs-generated cfg flags.
#[cfg(buddy_slab)]
mod buddy_slab;
#[cfg(not(any(tlsf, buddy_slab)))]
mod stub_impl;
#[cfg(tlsf)]
mod tlsf_impl;

#[cfg(buddy_slab)]
use buddy_slab as imp;
pub use imp::{
    DefaultByteAllocator, GlobalAllocator, global_add_memory, global_init, init_percpu_slab,
};
#[cfg(not(any(tlsf, buddy_slab)))]
use stub_impl as imp;
#[cfg(tlsf)]
use tlsf_impl as imp;

/// Returns the reference to the global allocator.
pub fn global_allocator() -> &'static GlobalAllocator {
    imp::global_allocator()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_memory_retries_after_reclaim_progress() {
        let mut attempts = 0;
        let mut reclaims = 0;
        let result = retry_after_page_reclaim(
            1,
            || {
                attempts += 1;
                (attempts == 2).then_some(42).ok_or(AllocError::NoMemory)
            },
            |target| {
                reclaims += 1;
                assert_eq!(target, MIN_RECLAIM_PAGES);
                1
            },
        );

        assert_eq!(result, Ok(42));
        assert_eq!(attempts, 2);
        assert_eq!(reclaims, 1);
    }

    #[test]
    fn zero_reclaim_progress_gets_one_concurrent_retry() {
        let mut attempts = 0;
        let mut reclaims = 0;
        let result = retry_after_page_reclaim::<()>(
            32,
            || {
                attempts += 1;
                Err(AllocError::NoMemory)
            },
            |target| {
                reclaims += 1;
                assert_eq!(target, 32);
                0
            },
        );

        assert_eq!(result, Err(AllocError::NoMemory));
        assert_eq!(attempts, 2);
        assert_eq!(reclaims, 1);
    }

    #[test]
    fn non_memory_error_does_not_enter_reclaim() {
        let mut reclaims = 0;
        let result = retry_after_page_reclaim::<()>(
            1,
            || Err(AllocError::InvalidParam),
            |_| {
                reclaims += 1;
                1
            },
        );

        assert_eq!(result, Err(AllocError::InvalidParam));
        assert_eq!(reclaims, 0);
    }

    #[test]
    fn reclaim_progress_has_a_bounded_retry_budget() {
        let mut attempts = 0;
        let mut reclaims = 0;
        let result = retry_after_page_reclaim::<()>(
            usize::MAX,
            || {
                attempts += 1;
                Err(AllocError::NoMemory)
            },
            |_| {
                reclaims += 1;
                1
            },
        );

        assert_eq!(result, Err(AllocError::NoMemory));
        assert_eq!(attempts, MAX_RECLAIM_ATTEMPTS + 1);
        assert_eq!(reclaims, MAX_RECLAIM_ATTEMPTS);
    }
}
