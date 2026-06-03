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

use core::{alloc::Layout, fmt, ptr::NonNull};

use ax_errno::AxError;
use strum::{IntoStaticStr, VariantArray};

const PAGE_SIZE: usize = 0x1000;

/// A function that tries to reclaim physical pages (e.g. by evicting
/// clean file-backed page cache pages). Returns the number of pages freed.
pub type PageReclaimFn = fn(num_pages: usize) -> usize;

static PAGE_RECLAIM_FN: ax_kspin::SpinNoIrq<Option<PageReclaimFn>> = ax_kspin::SpinNoIrq::new(None);

/// Register a callback that the allocator will invoke when a page allocation
/// cannot be satisfied.
pub fn register_page_reclaim_fn(f: PageReclaimFn) {
    *PAGE_RECLAIM_FN.lock() = Some(f);
}

/// Try to reclaim physical pages by invoking the registered callback.
/// Returns the number of pages actually freed.
///
/// The `SpinNoIrq` guard is released before calling into the reclaim
/// function so that the reclaim path (and any evict listeners it
/// triggers) runs with interrupts enabled.
pub fn try_page_reclaim(num_pages: usize) -> usize {
    let reclaim_fn = { *PAGE_RECLAIM_FN.lock() };
    reclaim_fn.map_or(0, |f| f(num_pages))
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AllocError {
    /// Invalid size, alignment, or other input parameter.
    InvalidParam,
    /// The allocator has already been initialized.
    AlreadyInitialized,
    /// A region overlaps with an existing managed region.
    MemoryOverlap,
    /// Not enough memory is available to satisfy the request.
    NoMemory,
    /// Attempted to deallocate memory that was not allocated.
    NotAllocated,
    /// The allocator has not been initialized.
    NotInitialized,
    /// The requested address or entity was not found.
    NotFound,
}

/// A [`Result`] alias with [`AllocError`] as the error type.
pub type AllocResult<T = ()> = Result<T, AllocError>;

impl From<AllocError> for AxError {
    fn from(value: AllocError) -> Self {
        match value {
            AllocError::NoMemory => AxError::NoMemory,
            AllocError::NotFound => AxError::NotFound,
            AllocError::NotInitialized | AllocError::AlreadyInitialized => AxError::BadState,
            AllocError::MemoryOverlap => AxError::AlreadyExists,
            AllocError::InvalidParam | AllocError::NotAllocated => AxError::InvalidInput,
        }
    }
}

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
