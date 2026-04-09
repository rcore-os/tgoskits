//! Memory allocator implementation backed by `buddy-slab-allocator`.

use core::{
    alloc::{GlobalAlloc, Layout},
    ptr::NonNull,
    slice,
};

use ax_kspin::SpinNoIrq;
use buddy_slab_allocator::{GlobalAllocator as InnerAllocator, OsImpl};

use super::{AllocResult, AllocatorOps, UsageKind, Usages};

/// The global allocator instance for buddy-slab mode.
#[cfg_attr(all(target_os = "none", not(test)), global_allocator)]
static GLOBAL_ALLOCATOR: GlobalAllocator = GlobalAllocator::new();

/// The default byte allocator for buddy-slab mode.
pub type DefaultByteAllocator = buddy_slab_allocator::SlabAllocator<PAGE_SIZE>;

const PAGE_SIZE: usize = 0x1000;

/// The global allocator used by ArceOS when `buddy-slab` is enabled.
pub struct GlobalAllocator {
    inner: SpinNoIrq<InnerAllocator<PAGE_SIZE>>,
    usages: SpinNoIrq<Usages>,
}

impl Default for GlobalAllocator {
    fn default() -> Self {
        Self::new()
    }
}

impl GlobalAllocator {
    /// Creates an empty [`GlobalAllocator`].
    pub const fn new() -> Self {
        Self {
            inner: SpinNoIrq::new(InnerAllocator::<PAGE_SIZE>::new()),
            usages: SpinNoIrq::new(Usages::new()),
        }
    }

    /// Returns the name of the allocator.
    pub const fn name(&self) -> &'static str {
        "buddy-slab-allocator"
    }

    /// Initializes the allocator with the given region.
    pub fn init(
        &self,
        start_vaddr: usize,
        size: usize,
        cpu_count: usize,
        os: &'static dyn OsImpl,
    ) -> AllocResult {
        info!(
            "Initialize global memory allocator, start_vaddr: {:#x}, size: {:#x}",
            start_vaddr, size
        );
        let region = unsafe { slice::from_raw_parts_mut(start_vaddr as *mut u8, size) };
        unsafe { self.inner.lock().init(region, cpu_count, os) }.map_err(Into::into)
    }

    /// Add the given region to the allocator.
    pub fn add_memory(&self, start_vaddr: usize, size: usize) -> AllocResult {
        info!(
            "Add memory region, start_vaddr: {:#x}, size: {:#x}",
            start_vaddr, size
        );
        let region = unsafe { slice::from_raw_parts_mut(start_vaddr as *mut u8, size) };
        unsafe { self.inner.lock().add_region(region) }.map_err(Into::into)
    }

    /// Allocate arbitrary number of bytes. Returns the left bound of the
    /// allocated region.
    pub fn alloc(&self, layout: Layout) -> AllocResult<NonNull<u8>> {
        let result = self
            .inner
            .lock()
            .alloc(layout)
            .map_err(crate::AllocError::from);
        if result.is_ok() {
            self.usages.lock().alloc(UsageKind::RustHeap, layout.size());
        }
        result
    }

    /// Gives back the allocated region to the byte allocator.
    pub fn dealloc(&self, pos: NonNull<u8>, layout: Layout) {
        self.usages
            .lock()
            .dealloc(UsageKind::RustHeap, layout.size());
        unsafe { self.inner.lock().dealloc(pos, layout) };
    }

    /// Allocates contiguous pages.
    pub fn alloc_pages(
        &self,
        num_pages: usize,
        alignment: usize,
        kind: UsageKind,
    ) -> AllocResult<usize> {
        let result = self
            .inner
            .lock()
            .alloc_pages(num_pages, alignment)
            .map_err(crate::AllocError::from);
        if result.is_ok() {
            self.usages.lock().alloc(kind, num_pages * PAGE_SIZE);
        }
        result
    }

    /// Allocates contiguous low-memory pages (physical address < 4 GiB).
    pub fn alloc_dma32_pages(
        &self,
        num_pages: usize,
        alignment: usize,
        kind: UsageKind,
    ) -> AllocResult<usize> {
        let result = self
            .inner
            .lock()
            .alloc_pages_lowmem(num_pages, alignment)
            .map_err(crate::AllocError::from);
        if result.is_ok() {
            self.usages.lock().alloc(kind, num_pages * PAGE_SIZE);
        }
        result
    }

    /// Allocates contiguous pages starting from the given address.
    pub fn alloc_pages_at(
        &self,
        _start: usize,
        _num_pages: usize,
        _alignment: usize,
        _kind: UsageKind,
    ) -> AllocResult<usize> {
        unimplemented!("buddy-slab allocator does not support alloc_pages_at")
    }

    /// Gives back the allocated pages starts from `pos` to the page allocator.
    pub fn dealloc_pages(&self, pos: usize, num_pages: usize, kind: UsageKind) {
        self.usages.lock().dealloc(kind, num_pages * PAGE_SIZE);
        self.inner.lock().dealloc_pages(pos, num_pages);
    }

    /// Returns the number of allocated bytes in the allocator backend.
    pub fn used_bytes(&self) -> usize {
        self.inner.lock().allocated_bytes()
    }

    /// Returns the number of available bytes in the allocator backend.
    pub fn available_bytes(&self) -> usize {
        let inner = self.inner.lock();
        inner
            .managed_bytes()
            .saturating_sub(inner.allocated_bytes())
    }

    /// Returns the number of allocated pages in the allocator backend.
    pub fn used_pages(&self) -> usize {
        self.used_bytes() / PAGE_SIZE
    }

    /// Returns the number of available pages in the allocator backend.
    pub fn available_pages(&self) -> usize {
        self.available_bytes() / PAGE_SIZE
    }

    /// Returns the usage statistics of the allocator.
    pub fn usages(&self) -> Usages {
        *self.usages.lock()
    }
}

impl AllocatorOps for GlobalAllocator {
    fn name(&self) -> &'static str {
        GlobalAllocator::name(self)
    }

    fn init(
        &self,
        start_vaddr: usize,
        size: usize,
        cpu_count: usize,
        os: &'static dyn OsImpl,
    ) -> AllocResult {
        GlobalAllocator::init(self, start_vaddr, size, cpu_count, os)
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

/// Initializes the global allocator with the given memory region.
pub fn global_init(
    start_vaddr: usize,
    size: usize,
    cpu_count: usize,
    os: &'static dyn OsImpl,
) -> AllocResult {
    debug!(
        "initialize global allocator at: [{:#x}, {:#x})",
        start_vaddr,
        start_vaddr + size
    );
    GLOBAL_ALLOCATOR.init(start_vaddr, size, cpu_count, os)?;
    info!("global allocator initialized");
    Ok(())
}

/// Add the given memory region to the global allocator.
pub fn global_add_memory(start_vaddr: usize, size: usize) -> AllocResult {
    debug!(
        "add a memory region to global allocator: [{:#x}, {:#x})",
        start_vaddr,
        start_vaddr + size
    );
    GLOBAL_ALLOCATOR.add_memory(start_vaddr, size)
}

unsafe impl GlobalAlloc for GlobalAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let inner = move || {
            if let Ok(ptr) = GlobalAllocator::alloc(self, layout) {
                ptr.as_ptr()
            } else {
                alloc::alloc::handle_alloc_error(layout)
            }
        };

        #[cfg(feature = "tracking")]
        {
            crate::tracking::with_state(|state| match state {
                None => inner(),
                Some(state) => {
                    let ptr = inner();
                    let generation = state.generation;
                    state.generation += 1;
                    state.map.insert(
                        ptr as usize,
                        crate::tracking::AllocationInfo {
                            layout,
                            backtrace: axbacktrace::Backtrace::capture(),
                            generation,
                        },
                    );
                    ptr
                }
            })
        }

        #[cfg(not(feature = "tracking"))]
        inner()
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let ptr = NonNull::new(ptr).expect("dealloc null ptr");
        let inner = || GlobalAllocator::dealloc(self, ptr, layout);

        #[cfg(feature = "tracking")]
        crate::tracking::with_state(|state| match state {
            None => inner(),
            Some(state) => {
                let address = ptr.as_ptr() as usize;
                state.map.remove(&address);
                inner()
            }
        });

        #[cfg(not(feature = "tracking"))]
        inner();
    }
}

impl From<buddy_slab_allocator::AllocError> for super::AllocError {
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
