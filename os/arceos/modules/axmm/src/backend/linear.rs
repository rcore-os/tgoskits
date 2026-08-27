use ax_hal::paging::{MappingFlags, PageTable, PagingError};
use ax_memory_addr::{MemoryAddr, PAGE_SIZE_4K, PhysAddr, VirtAddr};

use super::Backend;
use crate::tlb::TlbGather;

impl Backend {
    /// Creates a new linear mapping backend.
    pub const fn new_linear(pa_va_offset: usize) -> Self {
        Self::Linear { pa_va_offset }
    }

    pub(crate) const fn new_boot_linear(pa_va_offset: usize) -> Self {
        Self::BootLinear { pa_va_offset }
    }

    pub(crate) fn map_linear(
        &self,
        start: VirtAddr,
        size: usize,
        flags: MappingFlags,
        pt: &mut PageTable,
        pa_va_offset: usize,
        allow_huge: bool,
    ) -> bool {
        let va_to_pa = |va: VirtAddr| PhysAddr::from(va.as_usize() - pa_va_offset);
        debug!(
            "map_linear: [{:#x}, {:#x}) -> [{:#x}, {:#x}) {:?}",
            start,
            start + size,
            va_to_pa(start),
            va_to_pa(start + size),
            flags
        );
        pt.map_region(start, va_to_pa, size, flags, allow_huge)
            .is_ok()
    }

    pub(crate) fn unmap_linear(
        &self,
        start: VirtAddr,
        size: usize,
        gather: &mut TlbGather,
        pt: &mut PageTable,
        _pa_va_offset: usize,
    ) -> bool {
        debug!("unmap_linear: [{:#x}, {:#x})", start, start + size);
        if pt.unmap(start, size).is_err() {
            return false;
        }
        gather.invalidate(start, size);
        true
    }

    pub(crate) fn validate_linear_unmap(
        &self,
        start: VirtAddr,
        size: usize,
        pt: &PageTable,
    ) -> bool {
        validate_linear_unmap_layout(start, size, |addr| match pt.query_occupied(addr) {
            Ok((_, _, page_size)) => LinearLeaf::Mapped(page_size),
            Err(PagingError::NotMapped) => LinearLeaf::Unmapped,
            Err(_) => LinearLeaf::Invalid,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LinearLeaf {
    Unmapped,
    Mapped(usize),
    Invalid,
}

fn validate_linear_unmap_layout(
    start: VirtAddr,
    size: usize,
    mut query: impl FnMut(VirtAddr) -> LinearLeaf,
) -> bool {
    let Some(end) = start.as_usize().checked_add(size).map(VirtAddr::from_usize) else {
        return false;
    };
    let mut cursor = start;
    while cursor < end {
        match query(cursor) {
            LinearLeaf::Unmapped => cursor += PAGE_SIZE_4K,
            LinearLeaf::Mapped(page_size) => {
                if page_size < PAGE_SIZE_4K || !page_size.is_power_of_two() {
                    return false;
                }
                let leaf_start = cursor.align_down(page_size);
                let Some(leaf_end) = leaf_start
                    .as_usize()
                    .checked_add(page_size)
                    .map(VirtAddr::from_usize)
                else {
                    return false;
                };
                if leaf_start < start || leaf_end > end {
                    return false;
                }
                cursor = leaf_end;
            }
            LinearLeaf::Invalid => return false,
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    const HUGE_PAGE_SIZE: usize = 2 * 1024 * 1024;

    #[test]
    fn aligned_whole_huge_leaf_is_a_valid_linear_unmap() {
        let start = VirtAddr::from_usize(0x4000_0000);

        assert!(validate_linear_unmap_layout(start, HUGE_PAGE_SIZE, |_| {
            LinearLeaf::Mapped(HUGE_PAGE_SIZE)
        }));
    }

    #[test]
    fn partial_huge_leaf_is_rejected_before_any_pte_is_removed() {
        let leaf_start = VirtAddr::from_usize(0x4000_0000);
        let start = leaf_start + PAGE_SIZE_4K;

        assert!(!validate_linear_unmap_layout(
            start,
            HUGE_PAGE_SIZE - PAGE_SIZE_4K,
            |_| LinearLeaf::Mapped(HUGE_PAGE_SIZE),
        ));
    }
}
