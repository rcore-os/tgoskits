//! TGOSKits runtime memory allocator.
//!
//! It provides [`GlobalAllocator`], which implements the trait
//! [`core::alloc::GlobalAlloc`]. A static global variable of type
//! [`GlobalAllocator`] is defined with the `#[global_allocator]` attribute, to
//! be registered as the standard library's default allocator.

#![no_std]

extern crate alloc;

use core::{
    fmt,
    sync::atomic::{AtomicUsize, Ordering},
};

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

/// Physical page allocation zones exposed by the runtime allocator.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryZone {
    /// General-purpose memory.
    Normal,
    /// Memory addressable by devices with a 32-bit DMA mask.
    Dma32,
}

/// A contiguous page allocation request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageRequest {
    /// Number of 4-KiB pages.
    pub count: usize,
    /// Required physical-address alignment in bytes.
    pub align: usize,
    /// Required physical memory zone.
    pub zone: MemoryZone,
}

/// Metadata required to return a contiguous page allocation to its source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageRelease {
    /// Number of 4-KiB pages in the original allocation.
    pub count: usize,
    /// Physical memory zone that supplied the allocation.
    pub zone: MemoryZone,
}

impl From<PageRequest> for PageRelease {
    fn from(request: PageRequest) -> Self {
        Self {
            count: request.count,
            zone: request.zone,
        }
    }
}

/// Source used to satisfy an allocation.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, VariantArray)]
pub enum AllocationSource {
    /// General-purpose buddy allocator.
    Normal,
    /// Low-memory buddy allocator.
    Dma32,
}

impl From<MemoryZone> for AllocationSource {
    fn from(zone: MemoryZone) -> Self {
        match zone {
            MemoryZone::Normal => Self::Normal,
            MemoryZone::Dma32 => Self::Dma32,
        }
    }
}

/// Snapshot of allocation counters by source and usage.
#[derive(Clone, Copy)]
pub struct AllocatorStats([[usize; UsageKind::VARIANTS.len()]; AllocationSource::VARIANTS.len()]);

impl AllocatorStats {
    const fn new() -> Self {
        Self([[0; UsageKind::VARIANTS.len()]; AllocationSource::VARIANTS.len()])
    }

    /// Returns bytes attributed to one allocation source and usage kind.
    pub fn source_usage(&self, source: AllocationSource, kind: UsageKind) -> usize {
        self.0[source as usize][kind as usize]
    }

    /// Returns bytes attributed to a usage kind across all sources.
    pub fn usage(&self, kind: UsageKind) -> usize {
        AllocationSource::VARIANTS
            .iter()
            .map(|&source| self.source_usage(source, kind))
            .sum()
    }

    /// Returns bytes attributed to one allocation source across all usages.
    pub fn source(&self, source: AllocationSource) -> usize {
        UsageKind::VARIANTS
            .iter()
            .map(|&kind| self.source_usage(source, kind))
            .sum()
    }

    /// Returns all currently attributed allocation bytes.
    pub fn total(&self) -> usize {
        AllocationSource::VARIANTS
            .iter()
            .map(|&source| self.source(source))
            .sum()
    }
}

struct AllocatorCounters(
    [[AtomicUsize; UsageKind::VARIANTS.len()]; AllocationSource::VARIANTS.len()],
);

impl AllocatorCounters {
    const fn new() -> Self {
        Self(
            [const { [const { AtomicUsize::new(0) }; UsageKind::VARIANTS.len()] };
                AllocationSource::VARIANTS.len()],
        )
    }

    fn alloc(&self, source: AllocationSource, kind: UsageKind, size: usize) {
        self.0[source as usize][kind as usize].fetch_add(size, Ordering::Relaxed);
    }

    fn dealloc(&self, source: AllocationSource, kind: UsageKind, size: usize) {
        self.0[source as usize][kind as usize].fetch_sub(size, Ordering::Relaxed);
    }

    fn snapshot(&self) -> AllocatorStats {
        let mut snapshot = AllocatorStats::new();
        for &source in AllocationSource::VARIANTS {
            for &kind in UsageKind::VARIANTS {
                snapshot.0[source as usize][kind as usize] =
                    self.0[source as usize][kind as usize].load(Ordering::Relaxed);
            }
        }
        snapshot
    }
}

impl fmt::Debug for AllocatorStats {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut d = f.debug_struct("AllocatorStats");
        for &kind in UsageKind::VARIANTS {
            d.field(kind.into(), &self.usage(kind));
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
    #[error("allocator memory region overlaps an existing region")]
    MemoryOverlap,
    /// Not enough memory is available to satisfy the request.
    #[error("allocator has insufficient memory")]
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

mod buddy_slab;
use buddy_slab as imp;
pub use imp::{
    GlobalAllocator, global_add_memory, global_allocator, global_init, init_percpu_slab,
};

/// Allocates contiguous pages from the requested zone.
pub fn alloc_pages(request: PageRequest, usage: UsageKind) -> AllocResult<GlobalPage> {
    GlobalPage::allocate(request, usage)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocator_stats_views_use_the_same_source_usage_buckets() {
        let counters = AllocatorCounters::new();
        counters.alloc(AllocationSource::Normal, UsageKind::RustHeap, 128);
        counters.alloc(AllocationSource::Normal, UsageKind::PageTable, 4096);
        counters.alloc(AllocationSource::Dma32, UsageKind::Dma, 8192);

        let snapshot = counters.snapshot();
        assert_eq!(
            snapshot.source_usage(AllocationSource::Normal, UsageKind::RustHeap),
            128
        );
        assert_eq!(snapshot.usage(UsageKind::Dma), 8192);
        assert_eq!(snapshot.source(AllocationSource::Normal), 4224);
        assert_eq!(snapshot.total(), 12_416);

        counters.dealloc(AllocationSource::Normal, UsageKind::RustHeap, 128);
        assert_eq!(counters.snapshot().total(), 12_288);
    }
    #[test]
    fn page_release_keeps_only_the_metadata_required_by_deallocation() {
        let request = PageRequest {
            count: 4,
            align: 0x20_0000,
            zone: MemoryZone::Dma32,
        };

        assert_eq!(
            PageRelease::from(request),
            PageRelease {
                count: 4,
                zone: MemoryZone::Dma32,
            }
        );
    }
}
