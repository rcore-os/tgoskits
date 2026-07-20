//! Slab allocator — bitmap-based with lock-free cross-CPU freeing.
//!
//! The [`SlabAllocator`] is a standalone component that manages object allocation
//! within pre-supplied slab pages.  It does **not** allocate pages itself; instead
//! it returns [`SlabAllocResult::NeedsSlab`] to request pages from the caller.
//!
//! Cross-CPU frees go through the lock-free [`SlabPageHeader::remote_free`] path.

pub mod cache;
pub mod page;
pub mod size_class;

use core::{alloc::Layout, ptr::NonNull};

use ax_kspin::SpinRaw as SpinMutex;
use cache::{CacheDeallocResult, SlabCache};
pub use page::SlabPageHeader;
pub use size_class::SizeClass;

use crate::error::{AllocError, AllocResult};

/// Result of a slab allocation attempt.
pub enum SlabAllocResult {
    /// Object successfully allocated.
    Allocated(NonNull<u8>),
    /// The slab cache for this size class has no free objects.
    /// The caller should allocate `pages` pages from the buddy allocator,
    /// call [`SlabAllocator::add_slab`], and retry.
    NeedsSlab { size_class: SizeClass, pages: usize },
}

/// Result of a slab deallocation.
pub enum SlabDeallocResult {
    /// Object freed, nothing else to do.
    Done,
    /// The slab page at `base` became empty and should be returned to the buddy.
    FreeSlab { base: usize, pages: usize },
}

/// Result of a pool-mediated slab deallocation.
pub enum SlabPoolDeallocResult {
    /// Object freed on the local CPU path.
    Done,
    /// Object was queued onto the owner's remote-free list.
    RemoteQueued,
    /// The slab page at `base` became empty and should be returned to the buddy.
    FreeSlab { base: usize, pages: usize },
}

/// Object-safe slab interface used by [`crate::GlobalAllocator`] EII hooks.
pub trait SlabTrait: Sync {
    /// Logical CPU id this slab belongs to.
    fn cpu_id(&self) -> usize;

    /// Page size used by this slab.
    fn page_size(&self) -> usize;

    /// Allocate one object.
    fn alloc(&self, layout: Layout) -> AllocResult<SlabAllocResult>;

    /// Register a freshly allocated slab page.
    fn add_slab(&self, size_class: SizeClass, base: usize, bytes: usize);

    /// Free an object on the owner CPU path.
    fn dealloc_local(&self, ptr: NonNull<u8>, layout: Layout) -> SlabDeallocResult;

    /// Free an object on the remote CPU path.
    fn dealloc_remote(&self, ptr: NonNull<u8>) {
        let owner_cpu = u16::try_from(self.cpu_id()).expect("CPU id exceeds slab owner range");
        unsafe { SlabPageHeader::remote_free_object(ptr, owner_cpu, self.page_size()) };
    }
}

/// Object-safe slab-pool interface used by [`crate::GlobalAllocator`] EII hooks.
pub trait SlabPoolTrait: Sync {
    /// Return the slab belonging to the current CPU.
    fn current_slab(&self) -> &dyn SlabTrait;

    /// Return the owner slab for the given CPU.
    fn owner_slab(&self, cpu_idx: usize) -> &dyn SlabTrait;

    /// Logical CPU id of the current CPU.
    fn current_cpu_id(&self) -> usize {
        self.current_slab().cpu_id()
    }

    /// Allocate one object from the current CPU's slab.
    fn alloc(&self, layout: Layout) -> AllocResult<SlabAllocResult> {
        self.current_slab().alloc(layout)
    }

    /// Register a freshly allocated slab page in the current CPU's slab.
    fn add_slab(&self, size_class: SizeClass, base: usize, bytes: usize) {
        self.current_slab().add_slab(size_class, base, bytes)
    }

    /// Free an object, routing to local or remote slab ownership as needed.
    fn dealloc(&self, ptr: NonNull<u8>, layout: Layout, owner_cpu: usize) -> SlabPoolDeallocResult {
        if owner_cpu == self.current_cpu_id() {
            match self.current_slab().dealloc_local(ptr, layout) {
                SlabDeallocResult::Done => SlabPoolDeallocResult::Done,
                SlabDeallocResult::FreeSlab { base, pages } => {
                    SlabPoolDeallocResult::FreeSlab { base, pages }
                }
            }
        } else {
            self.owner_slab(owner_cpu).dealloc_remote(ptr);
            SlabPoolDeallocResult::RemoteQueued
        }
    }
}

/// Convenience helpers for callback-style slab access.
pub trait SlabPoolExt: SlabPoolTrait {
    /// Access the current CPU's slab via a callback.
    fn with_current_slab<R>(&self, f: impl FnOnce(&dyn SlabTrait) -> R) -> R {
        f(self.current_slab())
    }

    /// Access the given owner's slab via a callback.
    fn with_owner_slab<R>(&self, cpu_idx: usize, f: impl FnOnce(&dyn SlabTrait) -> R) -> R {
        f(self.owner_slab(cpu_idx))
    }
}

impl<T: ?Sized + SlabPoolTrait> SlabPoolExt for T {}

/// Standalone slab allocator (one per CPU or standalone use).
pub struct SlabAllocator<const PAGE_SIZE: usize = 0x1000> {
    caches: [SlabCache; SizeClass::COUNT],
}

/// Default per-CPU slab wrapper used by EII integrators.
pub struct PerCpuSlab<const PAGE_SIZE: usize = 0x1000> {
    cpu_id: u16,
    inner: SpinMutex<SlabAllocator<PAGE_SIZE>>,
}

/// Default static slab-pool wrapper used by EII integrators.
pub struct StaticSlabPool<const PAGE_SIZE: usize = 0x1000, const N: usize = 1> {
    slabs: [PerCpuSlab<PAGE_SIZE>; N],
    current_cpu_id: fn() -> usize,
}

impl<const PAGE_SIZE: usize> PerCpuSlab<PAGE_SIZE> {
    /// Create an empty per-CPU slab wrapper for `cpu_id`.
    pub const fn new(cpu_id: u16) -> Self {
        Self {
            cpu_id,
            inner: SpinMutex::new(SlabAllocator::new()),
        }
    }

    /// Reset the inner slab allocator to an empty state.
    pub fn reset(&self) {
        *self.inner.lock() = SlabAllocator::new();
    }

    /// Return this slab's logical CPU id.
    pub const fn cpu_id(&self) -> usize {
        self.cpu_id as usize
    }

    /// Allocate one object.
    pub fn alloc(&self, layout: Layout) -> AllocResult<SlabAllocResult> {
        self.inner.lock().alloc(layout)
    }

    /// Register a freshly allocated slab page.
    pub fn add_slab(&self, size_class: SizeClass, base: usize, bytes: usize) {
        self.inner
            .lock()
            .add_slab(size_class, base, bytes, self.cpu_id);
    }

    /// Free an object on the owner CPU path.
    pub fn dealloc_local(&self, ptr: NonNull<u8>, layout: Layout) -> SlabDeallocResult {
        self.inner.lock().dealloc(ptr, layout)
    }

    /// Queue an object onto this slab's remote-free list.
    pub fn dealloc_remote(&self, ptr: NonNull<u8>) {
        unsafe { SlabPageHeader::remote_free_object(ptr, self.cpu_id, PAGE_SIZE) };
    }
}

impl<const PAGE_SIZE: usize, const N: usize> StaticSlabPool<PAGE_SIZE, N> {
    /// Create a static slab pool from pre-built per-CPU slabs and a CPU-id hook.
    pub const fn new(slabs: [PerCpuSlab<PAGE_SIZE>; N], current_cpu_id: fn() -> usize) -> Self {
        Self {
            slabs,
            current_cpu_id,
        }
    }
}

impl<const PAGE_SIZE: usize> SlabAllocator<PAGE_SIZE> {
    /// Create a new (empty) slab allocator.  No pages are owned yet.
    pub const fn new() -> Self {
        Self {
            caches: [
                SlabCache::new(SizeClass::Bytes8),
                SlabCache::new(SizeClass::Bytes16),
                SlabCache::new(SizeClass::Bytes32),
                SlabCache::new(SizeClass::Bytes64),
                SlabCache::new(SizeClass::Bytes128),
                SlabCache::new(SizeClass::Bytes256),
                SlabCache::new(SizeClass::Bytes512),
                SlabCache::new(SizeClass::Bytes1024),
                SlabCache::new(SizeClass::Bytes2048),
            ],
        }
    }
}

impl<const PAGE_SIZE: usize> Default for SlabAllocator<PAGE_SIZE> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const PAGE_SIZE: usize> SlabAllocator<PAGE_SIZE> {
    /// Try to allocate an object matching `layout`.
    ///
    /// If the matching cache is exhausted, [`SlabAllocResult::NeedsSlab`] is returned
    /// so the caller can supply pages and retry.
    pub fn alloc(&mut self, layout: Layout) -> AllocResult<SlabAllocResult> {
        let sc = SizeClass::from_layout(layout).ok_or(AllocError::InvalidParam)?;
        let cache = &mut self.caches[sc.index()];

        match cache.alloc_object::<PAGE_SIZE>() {
            Some(addr) => {
                // SAFETY: `addr` is non-null, aligned, and within a live slab page.
                let ptr = unsafe { NonNull::new_unchecked(addr as *mut u8) };
                Ok(SlabAllocResult::Allocated(ptr))
            }
            None => Ok(SlabAllocResult::NeedsSlab {
                size_class: sc,
                pages: sc.slab_pages(PAGE_SIZE),
            }),
        }
    }

    /// Free an object previously allocated with [`alloc`](Self::alloc).
    ///
    /// This is the **local** (owner-CPU) path.  Cross-CPU frees should go through
    /// [`SlabPageHeader::remote_free`] directly (see [`GlobalAllocator`]).
    pub fn dealloc(&mut self, ptr: NonNull<u8>, layout: Layout) -> SlabDeallocResult {
        let sc = SizeClass::from_layout(layout).expect("layout exceeds slab size");
        let cache = &mut self.caches[sc.index()];

        match cache.dealloc_object::<PAGE_SIZE>(ptr.as_ptr() as usize) {
            CacheDeallocResult::Done => SlabDeallocResult::Done,
            CacheDeallocResult::FreeSlab { base, pages } => {
                SlabDeallocResult::FreeSlab { base, pages }
            }
        }
    }

    /// Supply a freshly allocated slab page to the given size class.
    ///
    /// `base` is the virtual address of the page(s), `bytes` = pages × PAGE_SIZE.
    pub fn add_slab(&mut self, size_class: SizeClass, base: usize, bytes: usize, owner_cpu: u16) {
        self.caches[size_class.index()].add_slab(base, bytes, owner_cpu);
    }
}

impl<const PAGE_SIZE: usize> SlabTrait for PerCpuSlab<PAGE_SIZE> {
    fn cpu_id(&self) -> usize {
        PerCpuSlab::cpu_id(self)
    }

    fn page_size(&self) -> usize {
        PAGE_SIZE
    }

    fn alloc(&self, layout: Layout) -> AllocResult<SlabAllocResult> {
        PerCpuSlab::alloc(self, layout)
    }

    fn add_slab(&self, size_class: SizeClass, base: usize, bytes: usize) {
        PerCpuSlab::add_slab(self, size_class, base, bytes)
    }

    fn dealloc_local(&self, ptr: NonNull<u8>, layout: Layout) -> SlabDeallocResult {
        PerCpuSlab::dealloc_local(self, ptr, layout)
    }
}

impl<const PAGE_SIZE: usize, const N: usize> SlabPoolTrait for StaticSlabPool<PAGE_SIZE, N> {
    fn current_slab(&self) -> &dyn SlabTrait {
        &self.slabs[(self.current_cpu_id)()]
    }

    fn owner_slab(&self, cpu_idx: usize) -> &dyn SlabTrait {
        &self.slabs[cpu_idx]
    }
}
