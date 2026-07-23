use ax_hal::paging::{MappingFlags, PageTable};
use ax_memory_addr::{MemoryAddr, PhysAddr, VirtAddr};

use super::Backend;

impl Backend {
    /// Creates a new linear mapping backend.
    pub const fn new_linear(pa_va_offset: i128) -> Self {
        Self::Linear { pa_va_offset }
    }

    pub(super) fn linear_paddr(vaddr: VirtAddr, pa_va_offset: i128) -> Option<PhysAddr> {
        let paddr = (vaddr.as_usize() as i128).checked_sub(pa_va_offset)?;
        usize::try_from(paddr).ok().map(PhysAddr::from)
    }

    pub(crate) fn map_linear(
        &self,
        start: VirtAddr,
        size: usize,
        flags: MappingFlags,
        pt: &mut PageTable,
        pa_va_offset: i128,
    ) -> bool {
        let Some(pa_start) = Self::linear_paddr(start, pa_va_offset) else {
            return false;
        };
        let Some(pa_end) = start
            .checked_add(size)
            .and_then(|end| Self::linear_paddr(end, pa_va_offset))
        else {
            return false;
        };
        debug!(
            "map_linear: [{:#x}, {:#x}) -> [{:#x}, {:#x}) {:?}",
            start,
            start + size,
            pa_start,
            pa_end,
            flags
        );
        pt.cursor()
            .map_region(
                start,
                |vaddr| {
                    Self::linear_paddr(vaddr, pa_va_offset)
                        .expect("linear mapping range must be validated during prepare")
                },
                size,
                flags,
                false,
            )
            .is_ok()
    }

    pub(crate) fn unmap_linear(
        &self,
        start: VirtAddr,
        size: usize,
        pt: &mut PageTable,
        _pa_va_offset: i128,
    ) -> bool {
        debug!("unmap_linear: [{:#x}, {:#x})", start, start + size);
        pt.cursor().unmap_region(start, size).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_linear_addresses_with_negative_offset() {
        let vaddr = VirtAddr::from(0x1000);

        assert_eq!(
            Backend::linear_paddr(vaddr, -0x1000),
            Some(PhysAddr::from(0x2000))
        );
    }

    #[test]
    fn rejects_linear_physical_address_overflow() {
        let vaddr = VirtAddr::from(usize::MAX);

        assert_eq!(Backend::linear_paddr(vaddr, -1), None);
    }
}
