pub use ax_memory_addr::{PhysAddr, VirtAddr};

pub const KB: usize = 1024;
pub const MB: usize = 1024 * KB;
pub const GB: usize = 1024 * MB;

#[derive(thiserror::Error, Clone, PartialEq, Eq)]
pub enum PagingError {
    #[error("Memory allocation failed")]
    NoMemory,
    #[error("Address alignment error: {details}")]
    AlignmentError { details: &'static str },
    #[error(
        "Mapping conflict: virtual address {vaddr:#x} already mapped to physical address \
         {existing_paddr:#x}"
    )]
    MappingConflict {
        vaddr: VirtAddr,
        existing_paddr: PhysAddr,
    },
    #[error("Address overflow detected: {details}")]
    AddressOverflow { details: &'static str },
    #[error("Invalid mapping size: {details}")]
    InvalidSize { details: &'static str },
    #[error("Page table hierarchy error: {details}")]
    HierarchyError { details: &'static str },
    #[error("Invalid address range: {details}")]
    InvalidRange { details: &'static str },
    #[error("Address not mapped")]
    NotMapped,
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

impl core::fmt::Debug for PagingError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NoMemory => write!(f, "NoMemory"),
            Self::AlignmentError { details } => write!(f, "AlignmentError: {details}"),
            Self::MappingConflict {
                vaddr,
                existing_paddr,
            } => {
                write!(
                    f,
                    "MappingConflict: vaddr={:#x}, existing_paddr={:#x}",
                    vaddr.as_usize(),
                    existing_paddr.as_usize()
                )
            }
            Self::AddressOverflow { details } => write!(f, "AddressOverflow: {details}"),
            Self::InvalidSize { details } => write!(f, "InvalidSize: {details}"),
            Self::HierarchyError { details } => write!(f, "HierarchyError: {details}"),
            Self::InvalidRange { details } => write!(f, "InvalidRange: {details}"),
            Self::NotMapped => write!(f, "NotMapped"),
        }
    }
}

/// Page sizes supported by the page-table engine.
#[repr(usize)]
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum PageSize {
    /// Size of 4 kilobytes.
    Size4K = 0x1000,
    /// Size of 1 megabytes.
    Size1M = 0x10_0000,
    /// Size of 2 megabytes.
    Size2M = 0x20_0000,
    /// Size of 1 gigabytes.
    Size1G = 0x4000_0000,
}

impl PageSize {
    /// Whether this page size is larger than the base 4K page.
    pub const fn is_huge(self) -> bool {
        matches!(self, Self::Size1G | Self::Size2M | Self::Size1M)
    }

    /// Checks whether an address or length is aligned to this page size.
    pub const fn is_aligned(self, addr_or_size: usize) -> bool {
        ax_memory_addr::is_aligned(addr_or_size, self as usize)
    }
}

impl From<PageSize> for usize {
    #[inline]
    fn from(size: PageSize) -> usize {
        size as usize
    }
}
