//! [ArceOS](https://github.com/arceos-org/arceos) global memory allocator.
//!
//! It provides [`GlobalAllocator`], which implements the trait
//! [`core::alloc::GlobalAlloc`]. A static global variable of type
//! [`GlobalAllocator`] is defined with the `#[global_allocator]` attribute, to
//! be registered as the standard library's default allocator.

#![no_std]

#[macro_use]
extern crate log;
extern crate alloc;

use core::{alloc::Layout, fmt, ptr::NonNull};

use ax_errno::AxError;
use strum::{IntoStaticStr, VariantArray};

const PAGE_SIZE: usize = 0x1000;

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

    fn alloc(&mut self, kind: UsageKind, size: usize) {
        self.0[kind as usize] += size;
    }

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
            AllocError::NotInitialized => AxError::BadState,
            AllocError::MemoryOverlap => AxError::AlreadyExists,
            AllocError::InvalidParam | AllocError::NotAllocated => AxError::InvalidInput,
        }
    }
}

#[cfg(not(feature = "buddy-slab"))]
impl From<ax_allocator::AllocError> for AllocError {
    fn from(value: ax_allocator::AllocError) -> Self {
        match value {
            ax_allocator::AllocError::InvalidParam => Self::InvalidParam,
            ax_allocator::AllocError::MemoryOverlap => Self::MemoryOverlap,
            ax_allocator::AllocError::NoMemory => Self::NoMemory,
            ax_allocator::AllocError::NotAllocated => Self::NotAllocated,
        }
    }
}

#[cfg(feature = "buddy-slab")]
impl From<buddy_slab_allocator::AllocError> for AllocError {
    fn from(value: buddy_slab_allocator::AllocError) -> Self {
        match value {
            buddy_slab_allocator::AllocError::InvalidParam => Self::InvalidParam,
            buddy_slab_allocator::AllocError::MemoryOverlap => Self::MemoryOverlap,
            buddy_slab_allocator::AllocError::NoMemory => Self::NoMemory,
            buddy_slab_allocator::AllocError::NotAllocated => Self::NotAllocated,
            buddy_slab_allocator::AllocError::NotInitialized => Self::NotInitialized,
            buddy_slab_allocator::AllocError::NotFound => Self::NotFound,
        }
    }
}

#[cfg(feature = "buddy-slab")]
pub use buddy_slab_allocator::OsImpl;

#[cfg(not(feature = "buddy-slab"))]
pub trait OsImpl: Sync + Send {
    /// Return the index of the current CPU (0-based).
    fn current_cpu_idx(&self) -> usize;

    /// Translate a virtual address to a physical address.
    fn virt_to_phys(&self, vaddr: usize) -> usize;
}

/// Unified allocator operations provided by all `ax-alloc` backends.
pub trait AllocatorOps {
    /// Returns the allocator name.
    fn name(&self) -> &'static str;

    /// Initializes the allocator with the given region.
    fn init(
        &self,
        start_vaddr: usize,
        size: usize,
        cpu_count: usize,
        os: &'static dyn OsImpl,
    ) -> AllocResult;

    /// Adds an extra memory region to the allocator.
    fn add_memory(&self, start_vaddr: usize, size: usize) -> AllocResult;

    /// Allocates arbitrary bytes.
    fn alloc(&self, layout: Layout) -> AllocResult<NonNull<u8>>;

    /// Deallocates a prior byte allocation.
    fn dealloc(&self, pos: NonNull<u8>, layout: Layout);

    /// Allocates contiguous pages.
    fn alloc_pages(
        &self,
        num_pages: usize,
        align_pow2: usize,
        kind: UsageKind,
    ) -> AllocResult<usize>;

    /// Allocates contiguous DMA32 pages.
    fn alloc_dma32_pages(
        &self,
        num_pages: usize,
        align_pow2: usize,
        kind: UsageKind,
    ) -> AllocResult<usize>;

    /// Allocates contiguous pages starting from the given address.
    fn alloc_pages_at(
        &self,
        start: usize,
        num_pages: usize,
        align_pow2: usize,
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

// Select implementation based on features.
#[cfg(feature = "buddy-slab")]
mod buddy_slab;
#[cfg(feature = "buddy-slab")]
use buddy_slab as imp;

#[cfg(not(feature = "buddy-slab"))]
mod default_impl;
#[cfg(not(feature = "buddy-slab"))]
use default_impl as imp;
pub use imp::{DefaultByteAllocator, GlobalAllocator, global_add_memory, global_init};

/// Returns the reference to the global allocator.
pub fn global_allocator() -> &'static GlobalAllocator {
    imp::global_allocator()
}
