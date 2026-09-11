use ax_hal::paging::{MappingFlags, PageTable, PagingError};
use ax_memory_addr::{MemoryAddr, PAGE_SIZE_4K, PhysAddr, VirtAddr};

use super::Backend;
use crate::tlb::TlbGather;

impl Backend {
    /// Creates a new linear mapping backend.
    pub fn new_linear(start_vaddr: VirtAddr, start_paddr: PhysAddr) -> Self {
        Self::Linear {
            pa_to_va_delta: pa_to_va_delta(start_vaddr, start_paddr),
        }
    }

    pub(crate) fn new_boot_linear(start_vaddr: VirtAddr, start_paddr: PhysAddr) -> Self {
        Self::BootLinear {
            pa_to_va_delta: pa_to_va_delta(start_vaddr, start_paddr),
        }
    }

    pub(crate) fn map_linear(
        &self,
        start: VirtAddr,
        size: usize,
        flags: MappingFlags,
        pt: &mut PageTable,
        pa_to_va_delta: i128,
        allow_huge: bool,
    ) -> bool {
        let Some((end, start_paddr, end_paddr)) =
            linear_paddr(start, pa_to_va_delta).and_then(|start_paddr| {
                Some((
                    VirtAddr::from_usize(start.as_usize().checked_add(size)?),
                    start_paddr,
                    PhysAddr::from_usize(start_paddr.as_usize().checked_add(size)?),
                ))
            })
        else {
            return false;
        };
        debug!(
            "map_linear: [{:#x}, {:#x}) -> [{:#x}, {:#x}) {:?}",
            start, end, start_paddr, end_paddr, flags
        );
        pt.map_linear_pages(start, start_paddr, size, flags, allow_huge)
            .is_ok()
    }

    pub(crate) fn unmap_linear(
        &self,
        start: VirtAddr,
        size: usize,
        gather: &mut TlbGather,
        pt: &mut PageTable,
        _pa_to_va_delta: i128,
    ) -> bool {
        debug!("unmap_linear: [{:#x}, {:#x})", start, start + size);
        let end = start + size;
        let mut leaves = alloc::vec::Vec::new();
        let mut cursor = start;
        while cursor < end {
            match pt.query_occupied(cursor) {
                Ok((_, level)) => {
                    let Some(page_size) = pt.mapping_size_for_level(level) else {
                        return false;
                    };
                    leaves.push(cursor);
                    cursor = cursor.align_down(page_size) + page_size;
                }
                Err(PagingError::NotMapped) => cursor += PAGE_SIZE_4K,
                Err(_) => return false,
            }
        }
        if gather.prepare_page_table_reclaims(leaves.len()).is_err() {
            return false;
        }
        for vaddr in leaves {
            let (_, _, _, deferred_page_tables) = pt
                .unmap_page_deferred(vaddr)
                .expect("a preflighted linear leaf must remain mapped under the aspace lock");
            gather.defer_page_tables(deferred_page_tables);
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
            Ok((_, level)) => pt
                .mapping_size_for_level(level)
                .map_or(LinearLeaf::Invalid, LinearLeaf::Mapped),
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

fn pa_to_va_delta(start_vaddr: VirtAddr, start_paddr: PhysAddr) -> i128 {
    start_vaddr.as_usize() as i128 - start_paddr.as_usize() as i128
}

fn linear_paddr(vaddr: VirtAddr, pa_to_va_delta: i128) -> Option<PhysAddr> {
    let paddr = (vaddr.as_usize() as i128).checked_sub(pa_to_va_delta)?;
    usize::try_from(paddr).ok().map(PhysAddr::from_usize)
}

#[cfg(test)]
mod address_delta_tests {
    use super::*;

    #[test]
    fn linear_mapping_supports_virtual_address_below_physical_address() {
        let start_vaddr = VirtAddr::from_usize(0x1000);
        let start_paddr = PhysAddr::from_usize(0x4000_0000);
        let delta = pa_to_va_delta(start_vaddr, start_paddr);

        assert_eq!(linear_paddr(start_vaddr, delta), Some(start_paddr));
        assert_eq!(
            linear_paddr(start_vaddr + 0x3000, delta),
            Some(start_paddr + 0x3000)
        );
    }

    #[test]
    fn linear_mapping_supports_virtual_address_above_physical_address() {
        let start_vaddr = VirtAddr::from_usize(0x4000_0000);
        let start_paddr = PhysAddr::from_usize(0x1000);
        let delta = pa_to_va_delta(start_vaddr, start_paddr);

        assert_eq!(linear_paddr(start_vaddr, delta), Some(start_paddr));
        assert_eq!(
            linear_paddr(start_vaddr + 0x3000, delta),
            Some(start_paddr + 0x3000)
        );
    }

    #[test]
    fn linear_mapping_rejects_addresses_outside_physical_range() {
        assert_eq!(
            linear_paddr(VirtAddr::from_usize(0), 1),
            None,
            "a positive delta must not wrap below physical address zero"
        );
        assert_eq!(
            linear_paddr(VirtAddr::from_usize(usize::MAX), -1),
            None,
            "a negative delta must not wrap beyond the physical address width"
        );
    }
}
