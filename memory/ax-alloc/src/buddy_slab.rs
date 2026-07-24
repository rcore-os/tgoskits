//! Runtime allocator implementation backed by `buddy-slab-allocator`.

use core::{
    alloc::{GlobalAlloc, Layout},
    ptr::NonNull,
    slice,
};

use ax_kernel_guard::NoPreempt;
use ax_kspin::SpinNoIrq;
use buddy_slab_allocator::{
    GlobalAllocator as InnerAllocator, SizeClass, SlabAllocResult, SlabAllocator,
    SlabDeallocResult, SlabPoolTrait, SlabTrait,
    eii::{slab_pool_impl, virt_to_phys_impl},
};
use log::{debug, info};

use super::{AllocResult, MemoryZone, PageRequest, UsageKind};
#[cfg(feature = "stats")]
use super::{AllocatorCounters, AllocatorStats};

/// The global allocator instance for buddy-slab mode.
#[cfg_attr(
    all(any(target_os = "none", feature = "global-allocator"), not(test)),
    global_allocator
)]
static GLOBAL_ALLOCATOR: GlobalAllocator = GlobalAllocator::new();

const PAGE_SIZE: usize = 0x1000;

#[ax_percpu::def_percpu]
static PERCPU_SLAB: PercpuSlab<PAGE_SIZE> = PercpuSlab::new_uninit();

static SLAB_POOL: SlabPool = SlabPool;

struct PercpuSlab<const PAGE_SIZE: usize = 0x1000> {
    cpu_id: Option<u16>,
    inner: SpinNoIrq<SlabAllocator<PAGE_SIZE>>,
}

impl<const PAGE_SIZE: usize> PercpuSlab<PAGE_SIZE> {
    const fn new_uninit() -> Self {
        Self {
            cpu_id: None,
            inner: SpinNoIrq::new(SlabAllocator::new()),
        }
    }

    fn init_during_cpu_bringup(&mut self, cpu_id: usize) {
        let cpu_id = u16::try_from(cpu_id).expect("CPU id exceeds per-CPU slab range");
        assert!(
            self.cpu_id.is_none(),
            "per-CPU slab is already initialized on this CPU",
        );
        self.cpu_id = Some(cpu_id);
        *self.inner.get_mut() = SlabAllocator::new();
    }

    fn cpu_id_checked(&self) -> u16 {
        self.cpu_id
            .expect("per-CPU slab is not initialized on this CPU")
    }
}

impl<const PAGE_SIZE: usize> SlabTrait for PercpuSlab<PAGE_SIZE> {
    fn cpu_id(&self) -> usize {
        self.cpu_id_checked() as usize
    }

    fn page_size(&self) -> usize {
        PAGE_SIZE
    }

    fn alloc(&self, layout: Layout) -> buddy_slab_allocator::AllocResult<SlabAllocResult> {
        self.inner.lock().alloc(layout)
    }

    fn add_slab(&self, size_class: SizeClass, base: usize, bytes: usize) {
        self.inner
            .lock()
            .add_slab(size_class, base, bytes, self.cpu_id_checked());
    }

    fn dealloc_local(&self, ptr: NonNull<u8>, layout: Layout) -> SlabDeallocResult {
        self.inner.lock().dealloc(ptr, layout)
    }
}

fn current_percpu_slab() -> NonNull<PercpuSlab<PAGE_SIZE>> {
    // SAFETY: every runtime byte allocation holds NoPreempt until the upstream
    // allocator finishes using this pointer. CPU areas live until shutdown and
    // PercpuSlab serializes all interior mutation with its IRQ-safe lock.
    unsafe { ax_percpu::with_cpu_pin(|pin| PERCPU_SLAB.current_ptr(pin)) }
        .expect("allocator access requires an installed CPU area")
}

fn remote_percpu_slab(cpu_idx: usize) -> NonNull<PercpuSlab<PAGE_SIZE>> {
    let cpu_index = ax_percpu::CpuIndex::try_from(cpu_idx)
        .expect("allocator CPU index must fit the CPU-local ABI");
    let area = ax_percpu::area(cpu_index)
        .expect("allocator CPU index must name an initialized CPU-local area");
    PERCPU_SLAB.remote_ptr(area)
}

struct SlabPool;

impl SlabPoolTrait for SlabPool {
    fn current_slab(&self) -> &dyn SlabTrait {
        // SAFETY: CPU areas outlive the global pool, and the caller holds
        // NoPreempt while the returned trait borrow is used.
        unsafe { current_percpu_slab().as_ref() }
    }

    fn owner_slab(&self, cpu_idx: usize) -> &dyn SlabTrait {
        // SAFETY: the selected area is permanent and PercpuSlab serializes all
        // local and remote interior mutation through its IRQ-safe lock.
        unsafe { remote_percpu_slab(cpu_idx).as_ref() }
    }
}

#[slab_pool_impl]
fn slab_pool() -> &'static dyn SlabPoolTrait {
    &SLAB_POOL
}

#[virt_to_phys_impl]
fn virt_to_phys(vaddr: usize) -> usize {
    ax_plat::mem::virt_to_phys(vaddr.into()).as_usize()
}

/// Runtime allocator backed by Buddy pages and per-CPU Slab caches.
pub struct GlobalAllocator {
    inner: InnerAllocator<PAGE_SIZE>,
    #[cfg(feature = "stats")]
    stats: AllocatorCounters,
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
            inner: InnerAllocator::<PAGE_SIZE>::new(),
            #[cfg(feature = "stats")]
            stats: AllocatorCounters::new(),
        }
    }

    /// Returns the name of the allocator.
    pub const fn name(&self) -> &'static str {
        "buddy-slab-allocator"
    }

    /// Initializes the allocator with the given region.
    pub fn init(&self, start_vaddr: usize, size: usize) -> AllocResult {
        info!(
            "Initialize global memory allocator, start_vaddr: {:#x}, size: {:#x}",
            start_vaddr, size
        );
        validate_region(start_vaddr, size)?;
        // SAFETY: the caller transfers an exclusive free-memory region to the
        // allocator. `validate_region` proves pointer arithmetic cannot wrap.
        let region = unsafe { slice::from_raw_parts_mut(start_vaddr as *mut u8, size) };
        unsafe { self.inner.init(region) }.map_err(Into::into)
    }

    /// Add the given region to the allocator.
    pub fn add_memory(&self, start_vaddr: usize, size: usize) -> AllocResult {
        info!(
            "Add memory region, start_vaddr: {:#x}, size: {:#x}",
            start_vaddr, size
        );
        validate_region(start_vaddr, size)?;
        // SAFETY: the caller transfers an exclusive free-memory region to the
        // allocator. `validate_region` proves pointer arithmetic cannot wrap.
        let region = unsafe { slice::from_raw_parts_mut(start_vaddr as *mut u8, size) };
        unsafe { self.inner.add_region(region) }.map_err(Into::into)
    }

    /// Allocate arbitrary number of bytes. Returns the left bound of the
    /// allocated region.
    pub fn alloc(&self, layout: Layout) -> AllocResult<NonNull<u8>> {
        // Slab lookup obtains a pointer to the current CPU's cache. Keep the
        // task on that CPU until the complete upstream operation finishes.
        let _guard = NoPreempt::new();
        let result = self.inner.alloc(layout).map_err(crate::AllocError::from);
        #[cfg(feature = "stats")]
        if result.is_ok() {
            self.stats.alloc(UsageKind::RustHeap, layout.size());
        }
        result
    }

    /// Gives back the allocated region to the byte allocator.
    pub fn dealloc(&self, pos: NonNull<u8>, layout: Layout) {
        // The upstream allocator selects local or remote Slab ownership using
        // the current CPU. Prevent migration until that routing completes.
        let _guard = NoPreempt::new();
        unsafe { self.inner.dealloc(pos, layout) };
        #[cfg(feature = "stats")]
        self.stats.dealloc(UsageKind::RustHeap, layout.size());
    }

    /// Allocates contiguous pages.
    pub fn alloc_pages(&self, request: PageRequest, _kind: UsageKind) -> AllocResult<usize> {
        let _bytes = request
            .count
            .checked_mul(PAGE_SIZE)
            .ok_or(crate::AllocError::InvalidParam)?;
        if request.count == 0 {
            return Err(crate::AllocError::InvalidParam);
        }
        let result = match request.zone {
            MemoryZone::Normal => self.inner.alloc_pages(request.count, request.align),
            MemoryZone::Dma32 => self.inner.alloc_pages_lowmem(request.count, request.align),
        };
        let addr = result.map_err(crate::AllocError::from)?;
        #[cfg(feature = "stats")]
        self.stats.alloc(_kind, _bytes);
        Ok(addr)
    }

    /// Gives back a contiguous page allocation.
    ///
    /// # Safety
    ///
    /// `pos` must identify a live allocation returned by
    /// [`Self::alloc_pages`] with the original page count and `kind`.
    /// The allocation must not be accessed or released again after this call.
    pub unsafe fn dealloc_pages(&self, pos: usize, count: usize, _kind: UsageKind) {
        let _bytes = count
            .checked_mul(PAGE_SIZE)
            .expect("a live page allocation has a validated byte size");
        self.inner.dealloc_pages(pos, count);
        #[cfg(feature = "stats")]
        self.stats.dealloc(_kind, _bytes);
    }

    /// Returns the number of allocated bytes in the allocator backend.
    pub fn used_bytes(&self) -> usize {
        self.inner.allocated_bytes()
    }

    /// Returns the number of available bytes in the allocator backend.
    pub fn available_bytes(&self) -> usize {
        self.inner
            .managed_bytes()
            .saturating_sub(self.inner.allocated_bytes())
    }

    /// Returns the number of allocated pages in the allocator backend.
    pub fn used_pages(&self) -> usize {
        self.used_bytes() / PAGE_SIZE
    }

    /// Returns the number of available pages in the allocator backend.
    pub fn available_pages(&self) -> usize {
        self.available_bytes() / PAGE_SIZE
    }

    /// Returns an allocation statistics snapshot.
    #[cfg(feature = "stats")]
    pub fn stats(&self) -> AllocatorStats {
        self.stats.snapshot()
    }
}

fn validate_region(start: usize, size: usize) -> AllocResult {
    if size > isize::MAX as usize || start.checked_add(size).is_none() {
        return Err(crate::AllocError::InvalidParam);
    }
    Ok(())
}

/// Returns the reference to the global allocator.
pub fn global_allocator() -> &'static GlobalAllocator {
    &GLOBAL_ALLOCATOR
}

/// Initializes the per-CPU slab for the current CPU during CPU bring-up.
///
/// Must run after per-CPU storage is initialized and before scheduler, IPI, or
/// IRQ paths can allocate on this CPU.
pub fn init_percpu_slab(cpu_id: usize) {
    // SAFETY: CPU bring-up excludes migration, IRQ/re-entry, and remote access
    // until this CPU-local slab has been initialized.
    unsafe {
        ax_percpu::with_cpu_pin(|pin| {
            ax_percpu::with_exclusive_cpu(pin, |exclusive| {
                PERCPU_SLAB.with_current_mut(exclusive, |slab| slab.init_during_cpu_bringup(cpu_id))
            })
        })
    }
    .expect("per-CPU slab initialization requires an installed CPU area");
}

/// Initializes the global allocator with the given memory region.
pub fn global_init(start_vaddr: usize, size: usize) -> AllocResult {
    validate_region(start_vaddr, size)?;
    debug!(
        "initialize global allocator at: [{:#x}, {:#x})",
        start_vaddr,
        start_vaddr + size
    );
    GLOBAL_ALLOCATOR.init(start_vaddr, size)?;
    info!("global allocator initialized");
    Ok(())
}

/// Add the given memory region to the global allocator.
pub fn global_add_memory(start_vaddr: usize, size: usize) -> AllocResult {
    validate_region(start_vaddr, size)?;
    debug!(
        "add a memory region to global allocator: [{:#x}, {:#x})",
        start_vaddr,
        start_vaddr + size
    );
    GLOBAL_ALLOCATOR.add_memory(start_vaddr, size)
}

// SAFETY: allocations and deallocations are delegated to the synchronized
// allocator using the exact pointer/layout pairs required by `GlobalAlloc`.
unsafe impl GlobalAlloc for GlobalAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let inner = move || match GlobalAllocator::alloc(self, layout) {
            Ok(ptr) => ptr.as_ptr(),
            Err(_) => core::ptr::null_mut(),
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
            buddy_slab_allocator::AllocError::AlreadyInitialized => Self::AlreadyInitialized,
            buddy_slab_allocator::AllocError::MemoryOverlap => Self::MemoryOverlap,
            buddy_slab_allocator::AllocError::NoMemory => Self::NoMemory,
            buddy_slab_allocator::AllocError::NotAllocated => Self::NotAllocated,
            buddy_slab_allocator::AllocError::NotInitialized => Self::NotInitialized,
            buddy_slab_allocator::AllocError::NotFound => Self::NotFound,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocator_region_rejects_address_overflow() {
        assert_eq!(
            validate_region(usize::MAX - 0x1000, 0x2000),
            Err(crate::AllocError::InvalidParam)
        );
    }

    #[test]
    fn allocator_region_rejects_slice_lengths_above_isize_max() {
        assert_eq!(
            validate_region(0, isize::MAX as usize + 1),
            Err(crate::AllocError::InvalidParam)
        );
    }
}
