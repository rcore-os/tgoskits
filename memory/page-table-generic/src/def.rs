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

bitflags::bitflags! {
    /// Generic page mapping permissions and memory attributes.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct MappingFlags: usize {
        /// The memory is readable.
        const READ = 1 << 0;
        /// The memory is writable.
        const WRITE = 1 << 1;
        /// The memory is executable.
        const EXECUTE = 1 << 2;
        /// The memory is accessible from a lower-privileged context.
        const USER = 1 << 3;
        /// The memory is device memory.
        const DEVICE = 1 << 4;
        /// The memory is uncached.
        const UNCACHED = 1 << 5;
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

impl PteConfig {
    /// Builds a leaf mapping config from generic mapping flags.
    pub fn page(paddr: PhysAddr, flags: MappingFlags, is_huge: bool) -> Self {
        Self {
            paddr,
            valid: !flags.is_empty(),
            read: flags.contains(MappingFlags::READ),
            writable: flags.contains(MappingFlags::WRITE),
            executable: flags.contains(MappingFlags::EXECUTE),
            lower: flags.contains(MappingFlags::USER),
            dirty: flags.contains(MappingFlags::WRITE),
            global: !flags.contains(MappingFlags::USER),
            is_dir: false,
            huge: is_huge,
            mem_attr: if flags.contains(MappingFlags::DEVICE) {
                MemAttributes::Device
            } else if flags.contains(MappingFlags::UNCACHED) {
                MemAttributes::Uncached
            } else {
                MemAttributes::Normal
            },
        }
    }
}

impl From<PteConfig> for MappingFlags {
    fn from(config: PteConfig) -> Self {
        if !config.valid {
            return Self::empty();
        }

        let mut flags = Self::empty();
        if config.read {
            flags |= Self::READ;
        }
        if config.writable {
            flags |= Self::WRITE;
        }
        if config.executable {
            flags |= Self::EXECUTE;
        }
        if config.lower {
            flags |= Self::USER;
        }
        match config.mem_attr {
            MemAttributes::Device => flags |= Self::DEVICE,
            MemAttributes::Uncached => flags |= Self::UNCACHED,
            MemAttributes::Normal | MemAttributes::PerCpu => {}
        }
        flags
    }
}
