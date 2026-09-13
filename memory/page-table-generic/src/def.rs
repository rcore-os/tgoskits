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
    #[error("Huge split deposit is stale for virtual address {vaddr:#x}")]
    StaleHugeSplit { vaddr: VirtAddr },
    #[error("Page-table map deposit is stale for virtual address {vaddr:#x}")]
    StaleMapDeposit { vaddr: VirtAddr },
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

    pub fn stale_huge_split(vaddr: VirtAddr) -> Self {
        Self::StaleHugeSplit { vaddr }
    }

    pub fn stale_map_deposit(vaddr: VirtAddr) -> Self {
        Self::StaleMapDeposit { vaddr }
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
            Self::StaleHugeSplit { vaddr } => {
                write!(f, "StaleHugeSplit: vaddr={:#x}", vaddr.as_usize())
            }
            Self::StaleMapDeposit { vaddr } => {
                write!(f, "StaleMapDeposit: vaddr={:#x}", vaddr.as_usize())
            }
        }
    }
}
