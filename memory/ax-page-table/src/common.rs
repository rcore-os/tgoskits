use ax_memory_addr::{MemoryAddr, PAGE_SIZE_4K};
pub use ax_memory_addr::{PhysAddr, VirtAddr};

pub const KB: usize = 1024;
pub const MB: usize = 1024 * KB;
pub const GB: usize = 1024 * MB;

/// The page sizes supported by page-table implementations.
#[repr(usize)]
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum PageSize {
    /// Size of 4 kilobytes (2<sup>12</sup> bytes).
    Size4K = 0x1000,
    /// Size of 1 megabyte (2<sup>20</sup> bytes).
    Size1M = 0x10_0000,
    /// Size of 2 megabytes (2<sup>21</sup> bytes).
    Size2M = 0x20_0000,
    /// Size of 1 gigabyte (2<sup>30</sup> bytes).
    Size1G = 0x4000_0000,
}

impl PageSize {
    /// Whether this page size is considered huge (larger than 4 KiB).
    pub const fn is_huge(self) -> bool {
        matches!(self, Self::Size1G | Self::Size2M | Self::Size1M)
    }

    /// Checks whether a given address or size is aligned to the page size.
    pub const fn is_aligned(self, addr_or_size: usize) -> bool {
        ax_memory_addr::is_aligned(addr_or_size, self as usize)
    }

    /// Returns the offset of the address within the page size.
    pub const fn align_offset(self, addr: usize) -> usize {
        ax_memory_addr::align_offset(addr, self as usize)
    }
}

impl From<PageSize> for usize {
    #[inline]
    fn from(size: PageSize) -> usize {
        size as usize
    }
}

/// Physical-frame source used by every page-table execution mode.
///
/// The provider owns allocation policy. `ax-page-table` only requests and
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
    fn alloc_frames(&self, count: usize, _align: usize) -> Option<PhysAddr> {
        (count == 1).then(|| self.alloc_frame()).flatten()
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

/// Hardware scope of a page-table invalidation instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TlbScope {
    /// The instruction affects only the current processing element.
    Local,
    /// The architecture instruction broadcasts within the shareable domain.
    HardwareBroadcast,
    /// The implementation explicitly sends and waits for remote IPIs.
    RemoteIpi,
}

/// Architecture or platform capability used to invalidate stale translations.
pub trait TlbInvalidator<A: MemoryAddr>: Sync + Send {
    /// Scope guaranteed by [`Self::invalidate`].
    const SCOPE: TlbScope;

    /// Invalidates one address, or the entire translation context for `None`.
    fn invalidate(vaddr: Option<A>);

    /// Invalidates a batch of individual addresses.
    fn invalidate_list(vaddrs: &[A]) {
        for &vaddr in vaddrs {
            Self::invalidate(Some(vaddr));
        }
    }
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
    #[error("address alignment error: {details}")]
    AlignmentError { details: &'static str },
    #[error(
        "Mapping conflict: virtual address {vaddr:#x} already mapped to physical address \
         {existing_paddr:#x}"
    )]
    MappingConflict {
        vaddr: VirtAddr,
        existing_paddr: PhysAddr,
    },
    #[error("address overflow detected: {details}")]
    AddressOverflow { details: &'static str },
    #[error("invalid mapping size: {details}")]
    InvalidSize { details: &'static str },
    #[error("page table hierarchy error: {details}")]
    HierarchyError { details: &'static str },
    #[error("invalid address range: {details}")]
    InvalidRange { details: &'static str },
}

impl PagingError {
    pub fn alignment_error(msg: &'static str) -> Self {
        Self::AlignmentError { details: msg }
    }

    pub fn mapping_conflict(vaddr: VirtAddr, existing_paddr: PhysAddr) -> Self {
        Self::MappingConflict {
            vaddr,
            existing_paddr,
        }
    }

    pub fn address_overflow(msg: &'static str) -> Self {
        Self::AddressOverflow { details: msg }
    }

    pub fn invalid_size(msg: &'static str) -> Self {
        Self::InvalidSize { details: msg }
    }

    pub fn hierarchy_error(msg: &'static str) -> Self {
        Self::HierarchyError { details: msg }
    }

    pub fn invalid_range(msg: &'static str) -> Self {
        Self::InvalidRange { details: msg }
    }

    pub fn not_mapped() -> Self {
        Self::NotMapped
    }
}

bitflags::bitflags! {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct AccessFlags: usize {
        const READ = 1;
        const WRITE = 1<<2;
        const EXECUTE = 1<<3;
        const LOWER = 1<<4;
    }
}

#[derive(Default, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MemAttributes {
    #[default]
    Normal,
    PerCpu,
    Device,
    Uncached,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemConfig {
    pub access: AccessFlags,
    pub attrs: MemAttributes,
}

impl core::fmt::Display for MemConfig {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "{}{}{}{}|{:?}",
            if self.access.contains(AccessFlags::READ) {
                "R"
            } else {
                "-"
            },
            if self.access.contains(AccessFlags::WRITE) {
                "W"
            } else {
                "-"
            },
            if self.access.contains(AccessFlags::EXECUTE) {
                "X"
            } else {
                "-"
            },
            if self.access.contains(AccessFlags::LOWER) {
                "L"
            } else {
                "-"
            },
            self.attrs
        )
    }
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
