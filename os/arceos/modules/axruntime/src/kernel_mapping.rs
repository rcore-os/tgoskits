use core::ptr::NonNull;

use ax_hal::paging::MappingFlags;
use ax_memory_addr::{MemoryAddr, PAGE_SIZE_4K, PhysAddr, VirtAddr, VirtAddrRange};

use crate::{RuntimeError, RuntimeResult};

pub(crate) enum MappingTransactionError {
    NotStarted(RuntimeError),
}
/// Changes permissions for a published kernel mapping and synchronously
/// invalidates every CPU that can use the kernel page table.
pub fn protect_kernel_range(start: VirtAddr, size: usize, flags: MappingFlags) -> RuntimeResult {
    ax_mm::kernel_aspace()
        .lock()
        .protect(start, size, flags)
        .map_err(Into::into)
}

/// Maps one contiguous physical range into a caller-selected kernel VA.
pub fn map_kernel_range(
    start: VirtAddr,
    paddr: PhysAddr,
    size: usize,
    flags: MappingFlags,
) -> RuntimeResult {
    ax_mm::kernel_aspace()
        .lock()
        .map_linear(start, paddr, size, flags)
        .map_err(Into::into)
}

/// Allocates a free kernel VA range and anonymous backing frames atomically.
pub fn allocate_kernel_range(
    hint: VirtAddr,
    size: usize,
    flags: MappingFlags,
    populate: bool,
) -> RuntimeResult<VirtAddr> {
    if size == 0 || !size.is_multiple_of(PAGE_SIZE_4K) {
        return Err(
            ax_mm::MmError::InvalidInput("kernel allocation size is not page aligned").into(),
        );
    }
    let mut aspace = ax_mm::kernel_aspace().lock();
    let start = find_free_kernel_range(&mut aspace, hint, size)?;
    aspace.map_alloc(start, size, flags, populate)?;
    Ok(start)
}

/// Maps a list of physical pages into one contiguous kernel VA range.
///
/// A partial mapping is synchronously rolled back before an error is returned.
pub fn map_kernel_pages(
    hint: VirtAddr,
    pages: &[PhysAddr],
    flags: MappingFlags,
) -> RuntimeResult<VirtAddr> {
    let size = pages
        .len()
        .checked_mul(PAGE_SIZE_4K)
        .filter(|size| *size != 0)
        .ok_or(ax_mm::MmError::InvalidInput(
            "kernel page list is empty or overflows",
        ))?;
    if pages.iter().any(|page| !page.is_aligned_4k()) {
        return Err(
            ax_mm::MmError::InvalidInput("kernel page list contains an unaligned frame").into(),
        );
    }

    let mut aspace = ax_mm::kernel_aspace().lock();
    let start = find_free_kernel_range(&mut aspace, hint, size)?;
    aspace.map_linear_pages(start, pages, flags)?;
    Ok(start)
}

fn find_free_kernel_range(
    aspace: &mut ax_mm::AddrSpace,
    hint: VirtAddr,
    size: usize,
) -> RuntimeResult<VirtAddr> {
    let range = VirtAddrRange::new(aspace.base(), aspace.end());
    aspace
        .find_free_area(hint, size, range)
        .ok_or(ax_mm::MmError::NoMemory)
        .map_err(Into::into)
}

/// Removes a kernel mapping only after synchronous TLB confirmation.
pub fn unmap_kernel_range(start: VirtAddr, size: usize) -> RuntimeResult {
    ax_mm::kernel_aspace()
        .lock()
        .unmap(start, size)
        .map_err(Into::into)
}

/// Returns the flags and page size for a kernel mapping without exposing a
/// mutable page-table reference.
pub fn query_kernel_mapping(start: VirtAddr) -> RuntimeResult<(MappingFlags, usize)> {
    let (_, flags, page_size) = ax_mm::kernel_aspace()
        .lock()
        .page_table()
        .query(start)
        .map_err(|_| ax_mm::MmError::BadAddress)?;
    Ok((flags, page_size))
}

/// Handles one kernel page fault through the global address-space owner.
pub(crate) fn handle_kernel_page_fault(
    addr: VirtAddr,
    flags: ax_hal::trap::PageFaultFlags,
) -> bool {
    ax_mm::kernel_aspace().lock().handle_page_fault(addr, flags)
}

/// Retries deferred kernel mapping resources after CPU-footprint changes.
pub(crate) fn retry_kernel_tlb_reclaims() -> RuntimeResult {
    ax_mm::kernel_aspace()
        .lock()
        .retry_quarantined_tlb_reclaims()
        .map_err(Into::into)
}

pub(crate) fn map_dma_coherent_alias(
    paddr: PhysAddr,
    size: usize,
) -> Result<NonNull<u8>, MappingTransactionError> {
    map_alias_transaction(|| {
        ax_mm::kernel_aspace()
            .lock()
            .map_dma_coherent_alias(paddr, size)
            .map_err(Into::into)
    })
}

pub(crate) fn unmap_dma_coherent_alias(alias: NonNull<u8>, size: usize) -> RuntimeResult {
    ax_mm::kernel_aspace()
        .lock()
        .unmap_dma_coherent_alias(alias, size)
        .map_err(Into::into)
}

fn map_alias_transaction(
    map: impl FnOnce() -> RuntimeResult<NonNull<u8>>,
) -> Result<NonNull<u8>, MappingTransactionError> {
    map().map_err(MappingTransactionError::NotStarted)
}

#[cfg(test)]
mod tests {
    use ax_hal::cache::TlbShootdownError;

    use super::*;
    use crate::RuntimeError;

    #[test]
    fn alias_mapping_reports_preflight_quarantine_failure_as_not_started() {
        let not_started =
            map_alias_transaction(|| Err(RuntimeError::from(ax_mm::MmError::NoMemory)));
        assert!(matches!(
            not_started,
            Err(MappingTransactionError::NotStarted(RuntimeError::Mm(
                ax_mm::MmError::NoMemory
            )))
        ));

        let blocked = map_alias_transaction(|| {
            Err(RuntimeError::from(ax_mm::MmError::TlbShootdown(
                TlbShootdownError::Timeout,
            )))
        });
        assert!(matches!(
            blocked,
            Err(MappingTransactionError::NotStarted(RuntimeError::Mm(
                ax_mm::MmError::TlbShootdown(TlbShootdownError::Timeout)
            )))
        ));
    }
}
