use alloc::vec::Vec;

use ax_hal::mem::PhysAddr;
#[cfg(feature = "paging")]
use ax_hal::{mem::VirtAddr, paging::MappingFlags};
#[cfg(feature = "paging")]
use ax_memory_addr::VirtAddrRange;
use ax_sync::Mutex;
use uefi_raw::table::boot::{AllocateType, MemoryType};

#[cfg(feature = "paging")]
static ALLOCATED_PAGES: Mutex<Vec<(VirtAddr, usize)>> = Mutex::new(Vec::new());
static ALLOCATED_POOLS: Mutex<Vec<(usize, core::alloc::Layout)>> = Mutex::new(Vec::new());

/// Reason a [`free_pages`] request could not be honored.
#[derive(Debug)]
pub enum FreePagesError {
    /// The address does not match a tracked allocation.
    NotTracked,
    /// The page count does not match the tracked allocation exactly.
    InvalidPageCount,
    /// The address was tracked, but unmapping it failed.
    UnmapFailed,
}

pub fn alloc_pages(_alloc_type: AllocateType, _memory_type: MemoryType, count: usize) -> *mut u8 {
    // Mapping EFI pages requires the paging stack; without it the service is
    // simply unavailable.
    #[cfg(not(feature = "paging"))]
    {
        let _ = count;
        core::ptr::null_mut()
    }

    #[cfg(feature = "paging")]
    {
        // Reject zero-page requests and sizes whose byte length would wrap
        // before touching the paging stack; both are reported as a null
        // pointer (the caller maps them to parameter/resource errors).
        if count == 0 {
            return core::ptr::null_mut();
        }
        let Some(size) = count.checked_mul(4096) else {
            return core::ptr::null_mut();
        };
        let mut aspace = ax_mm::kernel_aspace().lock();
        // Map a fresh RWX region above the RAM linear-mapping window instead of
        // `protect`ing heap pages: protecting a range that shares a 2 MiB linear
        // mapping with the page tables themselves faults while the split is in
        // flight (the physical pages backing the PTE writes become unmapped).
        let hint = VirtAddr::from_usize(0x9000_0000);
        let limit = VirtAddrRange::from_start_size(VirtAddr::from_usize(0), usize::MAX);
        let va = aspace
            .find_free_area(hint, size, limit)
            .expect("no free VA for EFI pages");
        aspace
            .map_alloc(
                va,
                size,
                MappingFlags::READ | MappingFlags::WRITE | MappingFlags::EXECUTE,
                true,
            )
            .expect("failed to map EFI pages");
        ALLOCATED_PAGES.lock().push((va, size));
        va.as_mut_ptr()
    }
}

/// Releases a page allocation obtained from [`alloc_pages`].
///
/// The UEFI `FreePages` page count is part of the caller contract: only the
/// exact allocation may be released, so a partial or oversized count is an
/// error rather than a reason to unmap the whole tracked range.
#[cfg(feature = "paging")]
pub fn free_pages(addr: PhysAddr, pages: usize) -> Result<(), FreePagesError> {
    // The UEFI spec wants `AllocatePages` to report a physical address, but
    // ArceBoot keeps its page tables active for the payload and hands out the
    // mapping's virtual address so the payload can use it directly. Match
    // that same value back against the tracked allocations here.
    // Identity-mapping the allocations and reporting true physical addresses
    // is future work (the same caveat as the GOP FrameBufferBase).
    let va = VirtAddr::from_usize(addr.as_usize());
    let Some(expected_size) = pages.checked_mul(4096) else {
        return Err(FreePagesError::InvalidPageCount);
    };
    let mut tracked = ALLOCATED_PAGES.lock();
    let Some(idx) = tracked.iter().position(|(v, _)| *v == va) else {
        return Err(FreePagesError::NotTracked);
    };
    if tracked[idx].1 != expected_size {
        return Err(FreePagesError::InvalidPageCount);
    }
    let (_, size) = tracked.swap_remove(idx);
    drop(tracked);
    ax_mm::kernel_aspace()
        .lock()
        .unmap(va, size)
        .inspect_err(|e| error!("failed to unmap EFI pages at {:#x}: {:?}", va.as_usize(), e))
        .map_err(|_| FreePagesError::UnmapFailed)
}

/// Without the paging stack there are no tracked page allocations to free.
#[cfg(not(feature = "paging"))]
pub fn free_pages(_addr: PhysAddr, _pages: usize) -> Result<(), FreePagesError> {
    Err(FreePagesError::NotTracked)
}

pub fn allocate_pool(_memory_type: MemoryType, size: usize) -> *mut u8 {
    if size == 0 {
        return core::ptr::null_mut();
    }
    // UEFI requires at least 8-byte alignment for pool allocations.
    let layout = match core::alloc::Layout::from_size_align(size, 8) {
        Ok(l) => l,
        Err(_) => return core::ptr::null_mut(),
    };
    let ptr = match ax_alloc::global_allocator().alloc(layout) {
        Ok(nn) => nn.as_ptr(),
        Err(_) => return core::ptr::null_mut(),
    };
    ALLOCATED_POOLS.lock().push((ptr as usize, layout));
    ptr
}

pub fn free_pool(buffer: *mut u8) {
    if buffer.is_null() {
        return;
    }
    let addr = buffer as usize;
    let mut pools = ALLOCATED_POOLS.lock();
    if let Some(idx) = pools.iter().position(|(p, _)| *p == addr) {
        let (_, layout) = pools.swap_remove(idx);
        // Safety: pointer/layout came from our allocator.
        unsafe {
            ax_alloc::global_allocator().dealloc(core::ptr::NonNull::new_unchecked(buffer), layout)
        };
    }
}
