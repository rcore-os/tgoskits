use ax_hal::paging::{MappingFlags, PageTable};
use ax_memory_addr::{PhysAddr, VirtAddr};

use super::Backend;

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
        let Some((end, start_paddr, end_paddr)) = start
            .as_usize()
            .checked_sub(pa_va_offset)
            .map(PhysAddr::from_usize)
            .and_then(|start_paddr| {
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
        _pa_va_offset: usize,
    ) -> bool {
        debug!("unmap_linear: [{:#x}, {:#x})", start, start + size);
        pt.unmap(start, size).is_ok()
    }
}
