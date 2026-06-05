//! TLSF memory allocator implementation using the `rlsf` crate.

use core::{
    alloc::{GlobalAlloc, Layout},
    ptr::NonNull,
};

use ax_kspin::SpinNoIrq;
use rlsf::Tlsf;

use super::{AllocResult, AllocatorOps, UsageKind, Usages};

/// The global allocator instance for TLSF mode.
#[cfg_attr(not(test), global_allocator)]
static GLOBAL_ALLOCATOR: GlobalAllocator = GlobalAllocator::new();

const PAGE_SIZE: usize = 0x1000;

/// The default byte allocator for TLSF mode.
pub type DefaultByteAllocator = Tlsf<'static, u32, u32, 28, 32>;

struct TlsfInfo {
    tlsf: Tlsf<'static, u32, u32, 28, 32>,
    total_bytes: usize,
    used_bytes: usize,
}

impl TlsfInfo {
    const fn new() -> Self {
        Self {
            tlsf: Tlsf::new(),
            total_bytes: 0,
            used_bytes: 0,
        }
    }
}

/// The global allocator used by ArceOS when TLSF is enabled.
pub struct GlobalAllocator {
    inner: SpinNoIrq<TlsfInfo>,
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
            inner: SpinNoIrq::new(TlsfInfo::new()),
            usages: SpinNoIrq::new(Usages::new()),
        }
    }

    /// Returns the name of the allocator.
    pub const fn name(&self) -> &'static str {
        "TLSF"
    }

    /// Initializes the allocator with the given region.
    pub fn init(&self, start_vaddr: usize, size: usize) -> AllocResult {
        let mut inner = self.inner.lock();
        unsafe {
            let pool = core::slice::from_raw_parts_mut(start_vaddr as *mut u8, size);
            inner
                .tlsf
                .insert_free_block_ptr(NonNull::new(pool).unwrap())
                .unwrap();
        }
        inner.total_bytes = size;
        Ok(())
    }

    /// Add the given region to the allocator.
    pub fn add_memory(&self, start_vaddr: usize, size: usize) -> AllocResult {
        let mut inner = self.inner.lock();
        unsafe {
            let pool = core::slice::from_raw_parts_mut(start_vaddr as *mut u8, size);
            inner
                .tlsf
                .insert_free_block_ptr(NonNull::new(pool).unwrap())
                .ok_or(crate::AllocError::InvalidParam)?;
        }
        inner.total_bytes += size;
        Ok(())
    }

    /// Allocate arbitrary number of bytes.
    pub fn alloc(&self, layout: Layout) -> AllocResult<NonNull<u8>> {
        let ptr = self
            .inner
            .lock()
            .tlsf
            .allocate(layout)
            .ok_or(crate::AllocError::NoMemory)?;
        self.inner.lock().used_bytes += layout.size();
        self.usages.lock().alloc(UsageKind::RustHeap, layout.size());
        Ok(ptr)
    }

    /// Gives back the allocated region.
    pub fn dealloc(&self, pos: NonNull<u8>, layout: Layout) {
        unsafe {
            self.inner.lock().tlsf.deallocate(pos, layout.align());
        }
        self.inner.lock().used_bytes -= layout.size();
        self.usages
            .lock()
            .dealloc(UsageKind::RustHeap, layout.size());
    }

    /// Allocates contiguous pages by allocating page-aligned bytes from TLSF.
    pub fn alloc_pages(
        &self,
        num_pages: usize,
        alignment: usize,
        kind: UsageKind,
    ) -> AllocResult<usize> {
        let size = num_pages * PAGE_SIZE;
        let align = alignment.max(PAGE_SIZE);
        let layout =
            Layout::from_size_align(size, align).map_err(|_| crate::AllocError::InvalidParam)?;
        let ptr = self
            .inner
            .lock()
            .tlsf
            .allocate(layout)
            .ok_or(crate::AllocError::NoMemory)?;
        self.inner.lock().used_bytes += size;
        if !matches!(kind, UsageKind::RustHeap) {
            self.usages.lock().alloc(kind, size);
        }
        Ok(ptr.as_ptr() as usize)
    }

    /// Allocates contiguous low-memory pages (physical address < 4 GiB).
    pub fn alloc_dma32_pages(
        &self,
        _num_pages: usize,
        _alignment: usize,
        _kind: UsageKind,
    ) -> AllocResult<usize> {
        unimplemented!("TLSF allocator does not support alloc_dma32_pages")
    }

    /// Allocates contiguous pages starting from the given address.
    pub fn alloc_pages_at(
        &self,
        _start: usize,
        _num_pages: usize,
        _alignment: usize,
        _kind: UsageKind,
    ) -> AllocResult<usize> {
        unimplemented!("TLSF allocator does not support alloc_pages_at")
    }

    /// Gives back the allocated pages.
    pub fn dealloc_pages(&self, pos: usize, num_pages: usize, kind: UsageKind) {
        let size = num_pages * PAGE_SIZE;
        let ptr = NonNull::new(pos as *mut u8).expect("dealloc_pages null ptr");
        unsafe {
            self.inner.lock().tlsf.deallocate(ptr, PAGE_SIZE);
        }
        self.inner.lock().used_bytes -= size;
        self.usages.lock().dealloc(kind, size);
    }

    /// Returns the number of allocated bytes.
    pub fn used_bytes(&self) -> usize {
        self.inner.lock().used_bytes
    }

    /// Returns the number of available bytes.
    pub fn available_bytes(&self) -> usize {
        let inner = self.inner.lock();
        inner.total_bytes.saturating_sub(inner.used_bytes)
    }

    /// Returns the number of allocated pages.
    pub fn used_pages(&self) -> usize {
        self.used_bytes() / PAGE_SIZE
    }

    /// Returns the number of available pages.
    pub fn available_pages(&self) -> usize {
        self.available_bytes() / PAGE_SIZE
    }

    /// Returns the usage statistics.
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
/// TLSF does not use per-CPU slabs, so this is intentionally a no-op.
pub fn init_percpu_slab(_cpu_id: usize) {}

/// Initializes the global allocator with the given memory region.
pub fn global_init(start_vaddr: usize, size: usize) -> AllocResult {
    debug!(
        "initialize global allocator at: [{:#x}, {:#x})",
        start_vaddr,
        start_vaddr + size
    );
    GLOBAL_ALLOCATOR.init(start_vaddr, size)
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
