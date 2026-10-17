//! Kernel aliases with confirmed native page-table retirement.

use alloc::vec::Vec;
use core::ffi::c_void;

use ax_hal::paging::MappingFlags;
use ax_memory_addr::{PhysAddr, VirtAddr, VirtAddrRange};
use ax_mm::MmError;
use drm_kapi::{
    config::{EEXIST, EFAULT, EINVAL, ENOMEM, EOPNOTSUPP, PAGE_SIZE},
    gem_memory::PgProt,
    vm::{VmapCacheMode, VmapOps},
};

pub(super) static OPS: VmapOps = VmapOps {
    map_pages,
    unmap,
    // RDIF buffers carry their existing DMA mapping attributes. Adding a WC
    // alias to ordinary WB RAM needs a platform cache policy not supplied by
    // this adapter; reject before modifying a VMA or installing any PTE.
    pgprot_writecombine: |_prot: PgProt| Err(-EOPNOTSUPP),
};

fn map_error(error: MmError) -> i32 {
    match error {
        MmError::InvalidInput(_) => -EINVAL,
        MmError::NoMemory => -ENOMEM,
        MmError::AlreadyExists => -EEXIST,
        MmError::BadAddress => -EFAULT,
        MmError::Unsupported => -EOPNOTSUPP,
        MmError::BadState(_) | MmError::TlbShootdown(_) => {
            // The caller still owns the page references. An inconsistent
            // rollback cannot be reported as a clean failure that permits
            // those pages to be recycled behind a surviving mapping.
            panic!("DRM kernel mapping ownership is uncertain: {error}")
        }
    }
}

fn map_pages(pages: &[u64], mode: VmapCacheMode) -> Result<*mut c_void, i32> {
    if mode != VmapCacheMode::WriteBack {
        return Err(-EOPNOTSUPP);
    }
    let size = pages
        .len()
        .checked_mul(PAGE_SIZE)
        .filter(|size| *size != 0)
        .ok_or(-EINVAL)?;
    let mut physical = Vec::new();
    physical
        .try_reserve_exact(pages.len())
        .map_err(|_| -ENOMEM)?;
    for &page in pages {
        let page = usize::try_from(page).map_err(|_| -EINVAL)?;
        if !page.is_multiple_of(PAGE_SIZE) {
            return Err(-EINVAL);
        }
        physical.push(PhysAddr::from(page));
    }
    let mut aspace = ax_mm::kernel_aspace().lock_irqsave();
    let limit = VirtAddrRange::from_start_size(aspace.base(), aspace.size());
    let base = aspace
        .find_free_area(aspace.base(), size, limit)
        .ok_or(-ENOMEM)?;
    let mapped = aspace.map_linear_pages(base, &physical, MappingFlags::READ | MappingFlags::WRITE);
    // map_linear_pages preserves any pending TLB gather internally; its
    // return alone does not prove that every CPU acknowledged a rollback.
    // Confirm before returning either success or a recyclable error.
    aspace
        .retry_quarantined_tlb_reclaims()
        .unwrap_or_else(|error| panic!("DRM mapping requires confirmed TLB retirement: {error}"));
    mapped.map_err(map_error)?;
    Ok(base.as_usize() as *mut c_void)
}

fn unmap(base: *mut c_void, page_count: usize) -> Result<(), i32> {
    let size = page_count
        .checked_mul(PAGE_SIZE)
        .filter(|size| *size != 0)
        .ok_or(-EINVAL)?;
    let address = base as usize;
    if base.is_null() || !address.is_multiple_of(PAGE_SIZE) {
        return Err(-EINVAL);
    }
    let mut aspace = ax_mm::kernel_aspace().lock_irqsave();
    let unmapped = aspace.unmap(VirtAddr::from(address), size);
    // KAPI retains its mapping record/page owners until this callback succeeds.
    aspace
        .retry_quarantined_tlb_reclaims()
        .unwrap_or_else(|error| panic!("DRM unmap requires confirmed TLB retirement: {error}"));
    unmapped.map_err(map_error)
}
