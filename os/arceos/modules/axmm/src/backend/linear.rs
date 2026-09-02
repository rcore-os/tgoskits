use ax_hal::paging::{MappingFlags, PageTable};
use ax_memory_addr::{PhysAddr, VirtAddr};

use super::Backend;

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
        pt: &mut PageTable,
        _pa_to_va_delta: i128,
    ) -> bool {
        debug!("unmap_linear: [{:#x}, {:#x})", start, start + size);
        pt.unmap(start, size).is_ok()
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
mod tests {
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
