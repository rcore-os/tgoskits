//! Stub allocator implementation when no backend is enabled.

use core::{
    alloc::{GlobalAlloc, Layout},
    ptr::NonNull,
};

use ax_kspin::SpinNoIrq;

use super::{AllocResult, AllocatorOps, UsageKind, Usages};

/// The global allocator instance (stub).
#[cfg_attr(all(any(target_os = "none", arceos_std), not(test)), global_allocator)]
static GLOBAL_ALLOCATOR: GlobalAllocator = GlobalAllocator::new();

/// Placeholder byte allocator type when no backend is enabled.
pub type DefaultByteAllocator = ();

/// The global allocator stub when no backend is enabled.
pub struct GlobalAllocator {
    usages: SpinNoIrq<Usages>,
}

impl Default for GlobalAllocator {
    fn default() -> Self {
        Self::new()
    }
}

impl GlobalAllocator {
    /// Creates a new empty stub allocator.
    pub const fn new() -> Self {
        Self {
            usages: SpinNoIrq::new(Usages::new()),
        }
    }

    /// Returns the name of the allocator.
    pub const fn name(&self) -> &'static str {
        "stub"
    }

    /// Initializes the allocator (stub).
    pub fn init(&self, _start_vaddr: usize, _size: usize) -> AllocResult {
        unimplemented!("no allocator backend enabled, enable 'tlsf' or 'buddy-slab' feature")
    }

    /// Add memory (stub).
    pub fn add_memory(&self, _start_vaddr: usize, _size: usize) -> AllocResult {
        unimplemented!("no allocator backend enabled")
    }

    /// Allocate bytes (stub).
    pub fn alloc(&self, _layout: Layout) -> AllocResult<NonNull<u8>> {
        unimplemented!("no allocator backend enabled")
    }

    /// Deallocate bytes (stub).
    pub fn dealloc(&self, _pos: NonNull<u8>, _layout: Layout) {
        unimplemented!("no allocator backend enabled")
    }

    /// Allocate pages (stub).
    pub fn alloc_pages(
        &self,
        _num_pages: usize,
        _align: usize,
        _kind: UsageKind,
    ) -> AllocResult<usize> {
        unimplemented!("no allocator backend enabled")
    }

    /// Allocate DMA32 pages (stub).
    pub fn alloc_dma32_pages(
        &self,
        _num_pages: usize,
        _alignment: usize,
        _kind: UsageKind,
    ) -> AllocResult<usize> {
        unimplemented!("no allocator backend enabled")
    }

    /// Allocate pages at address (stub).
    pub fn alloc_pages_at(
        &self,
        _start: usize,
        _num_pages: usize,
        _alignment: usize,
        _kind: UsageKind,
    ) -> AllocResult<usize> {
        unimplemented!("no allocator backend enabled")
    }

    /// Deallocate pages (stub).
    pub fn dealloc_pages(&self, _pos: usize, _num_pages: usize, _kind: UsageKind) {
        unimplemented!("no allocator backend enabled")
    }

    /// Returns used bytes (stub).
    pub fn used_bytes(&self) -> usize {
        0
    }

    /// Returns available bytes (stub).
    pub fn available_bytes(&self) -> usize {
        0
    }

    /// Returns used pages (stub).
    pub fn used_pages(&self) -> usize {
        0
    }

    /// Returns available pages (stub).
    pub fn available_pages(&self) -> usize {
        0
    }

    /// Returns usage statistics.
    pub fn usages(&self) -> Usages {
        *self.usages.lock()
    }
}

impl AllocatorOps for GlobalAllocator {
    fn name(&self) -> &'static str {
        GlobalAllocator::name(self)
    }

    fn init(&self, start_vaddr: usize, size: usize) -> AllocResult {
        GlobalAllocator::init(self, start_vaddr, size)
    }

    fn add_memory(&self, start_vaddr: usize, size: usize) -> AllocResult {
        GlobalAllocator::add_memory(self, start_vaddr, size)
    }

    fn alloc(&self, layout: Layout) -> AllocResult<NonNull<u8>> {
        GlobalAllocator::alloc(self, layout)
    }

    fn dealloc(&self, pos: NonNull<u8>, layout: Layout) {
        GlobalAllocator::dealloc(self, pos, layout)
    }

    fn alloc_pages(
        &self,
        num_pages: usize,
        alignment: usize,
        kind: UsageKind,
    ) -> AllocResult<usize> {
        GlobalAllocator::alloc_pages(self, num_pages, alignment, kind)
    }

    fn alloc_dma32_pages(
        &self,
        num_pages: usize,
        alignment: usize,
        kind: UsageKind,
    ) -> AllocResult<usize> {
        GlobalAllocator::alloc_dma32_pages(self, num_pages, alignment, kind)
    }

    fn alloc_pages_at(
        &self,
        start: usize,
        num_pages: usize,
        alignment: usize,
        kind: UsageKind,
    ) -> AllocResult<usize> {
        GlobalAllocator::alloc_pages_at(self, start, num_pages, alignment, kind)
    }

    fn dealloc_pages(&self, pos: usize, num_pages: usize, kind: UsageKind) {
        GlobalAllocator::dealloc_pages(self, pos, num_pages, kind)
    }

    fn used_bytes(&self) -> usize {
        GlobalAllocator::used_bytes(self)
    }

    fn available_bytes(&self) -> usize {
        GlobalAllocator::available_bytes(self)
    }

    fn used_pages(&self) -> usize {
        GlobalAllocator::used_pages(self)
    }

    fn available_pages(&self) -> usize {
        GlobalAllocator::available_pages(self)
    }

    fn usages(&self) -> Usages {
        GlobalAllocator::usages(self)
    }
}

/// Returns the reference to the global allocator.
pub fn global_allocator() -> &'static GlobalAllocator {
    &GLOBAL_ALLOCATOR
}

/// Initializes per-CPU allocator state.
///
/// The stub backend has no per-CPU state.
pub fn init_percpu_slab(_cpu_id: usize) {}

/// Initializes the global allocator (stub).
pub fn global_init(start_vaddr: usize, size: usize) -> AllocResult {
    GLOBAL_ALLOCATOR.init(start_vaddr, size)
}

/// Add the given memory region to the global allocator (stub).
pub fn global_add_memory(start_vaddr: usize, size: usize) -> AllocResult {
    GLOBAL_ALLOCATOR.add_memory(start_vaddr, size)
}

unsafe impl GlobalAlloc for GlobalAllocator {
    unsafe fn alloc(&self, _layout: Layout) -> *mut u8 {
        unimplemented!("no allocator backend enabled")
    }

    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {
        unimplemented!("no allocator backend enabled")
    }
}
