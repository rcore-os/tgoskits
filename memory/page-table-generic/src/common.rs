use ax_memory_addr::PAGE_SIZE_4K;
pub use ax_memory_addr::{PageSize, PhysAddr, VirtAddr};

/// Physical-frame source used by every page-table execution mode.
///
/// The provider owns allocation policy. The page-table core only requests and
/// returns frames, so boot and runtime consumers can use different sources
/// without introducing a dependency on `ax-alloc`.
pub trait PageFrameProvider: Clone + Sync + Send + 'static {
    /// Byte size of one frame supplied by this provider.
    const FRAME_SIZE: usize = PAGE_SIZE_4K;

    /// Allocates one frame.
    fn alloc_frame(&self) -> Option<PhysAddr>;

    /// Returns one frame.
    fn dealloc_frame(&self, paddr: PhysAddr);

    /// Allocates contiguous frames with the requested byte alignment.
    fn alloc_frames(&self, count: usize, align: usize) -> Option<PhysAddr> {
        if count == 1 && align.is_power_of_two() && align <= Self::FRAME_SIZE {
            self.alloc_frame()
        } else {
            None
        }
    }

    /// Returns a frame range previously allocated by this provider.
    fn dealloc_frames(&self, start: PhysAddr, count: usize) {
        for index in 0..count {
            self.dealloc_frame(start + index * Self::FRAME_SIZE);
        }
    }

    /// Converts a physical address into an address usable by the page-table walker.
    fn phys_to_virt(&self, paddr: PhysAddr) -> VirtAddr;
}

/// Errors shared by boot, stage-1, and stage-2 page-table operations.
#[derive(thiserror::Error, Debug, Clone, Copy, PartialEq, Eq)]
pub enum PagingError {
    #[error("page-table frame allocation failed")]
    NoMemory,
    #[error("address is not aligned to the selected page size")]
    NotAligned,
    #[error("address is not mapped")]
    NotMapped,
    #[error("address is already mapped")]
    AlreadyMapped,
    #[error("mapping resolves through a huge-page entry")]
    MappedToHugePage,
    #[error("address overflow detected: {details}")]
    AddressOverflow { details: &'static str },
    #[error("invalid mapping size: {details}")]
    InvalidSize { details: &'static str },
    #[error("page table hierarchy error: {details}")]
    HierarchyError { details: &'static str },
    #[error("invalid address range: {details}")]
    InvalidRange { details: &'static str },
}

#[derive(Default, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MemAttributes {
    #[default]
    Normal,
    PerCpu,
    Device,
    Uncached,
}

#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
pub struct PteConfig {
    pub paddr: PhysAddr,
    pub valid: bool,
    pub read: bool,
    pub writable: bool,
    pub executable: bool,
    pub lower: bool,
    pub dirty: bool,
    pub global: bool,
    pub is_dir: bool,
    pub huge: bool,
    pub mem_attr: MemAttributes,
}

#[cfg(feature = "ax-errno")]
impl From<PagingError> for ax_errno::AxErrorKind {
    fn from(value: PagingError) -> Self {
        match value {
            PagingError::NoMemory => ax_errno::AxErrorKind::NoMemory,
            _ => ax_errno::AxErrorKind::InvalidInput,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone)]
    struct SingleFrameProvider;

    impl PageFrameProvider for SingleFrameProvider {
        fn alloc_frame(&self) -> Option<PhysAddr> {
            Some(PhysAddr::from(0x1000))
        }

        fn dealloc_frame(&self, _paddr: PhysAddr) {}

        fn phys_to_virt(&self, paddr: PhysAddr) -> VirtAddr {
            VirtAddr::from(paddr.as_usize())
        }
    }

    #[test]
    fn default_frame_allocation_rejects_stricter_alignment() {
        assert_eq!(SingleFrameProvider.alloc_frames(1, PAGE_SIZE_4K * 2), None);
    }
}
