use core::ptr::NonNull;

pub const KB: usize = 1024;
pub const MB: usize = 1024 * KB;
pub const GB: usize = 1024 * MB;

macro_rules! def_addr {
    ($name:ident, $t:ty) => {
        #[repr(transparent)]
        #[derive(Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
        pub struct $name($t);

        impl From<$t> for $name {
            #[inline(always)]
            fn from(value: $t) -> Self {
                Self(value)
            }
        }

        impl From<$name> for $t {
            #[inline(always)]
            fn from(value: $name) -> Self {
                value.0
            }
        }

        impl $name {
            #[inline(always)]
            pub fn raw(&self) -> $t {
                self.0
            }

            #[inline(always)]
            pub const fn new(value: $t) -> Self {
                Self(value)
            }
        }

        impl core::ops::Add<$t> for $name {
            type Output = Self;

            #[inline(always)]
            fn add(self, rhs: $t) -> Self::Output {
                Self(self.0 + rhs)
            }
        }

        impl core::ops::AddAssign<$t> for $name {
            #[inline(always)]
            fn add_assign(&mut self, rhs: $t) {
                self.0 += rhs;
            }
        }

        impl core::ops::Sub<$t> for $name {
            type Output = Self;

            #[inline(always)]
            fn sub(self, rhs: $t) -> Self::Output {
                Self(self.0 - rhs)
            }
        }

        impl core::ops::Sub<Self> for $name {
            type Output = $t;

            #[inline(always)]
            fn sub(self, rhs: Self) -> Self::Output {
                self.0 - rhs.0
            }
        }

        impl core::fmt::Debug for $name {
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                write!(f, "0x{:0>16x}", self.0)
            }
        }
    };
}

def_addr!(PhysAddr, usize);
def_addr!(VirtAddr, usize);

impl VirtAddr {
    #[inline(always)]
    pub fn as_ptr(self) -> *mut u8 {
        self.0 as _
    }
}

impl From<*mut u8> for VirtAddr {
    #[inline(always)]
    fn from(val: *mut u8) -> Self {
        Self(val as _)
    }
}

impl From<NonNull<u8>> for VirtAddr {
    #[inline(always)]
    fn from(val: NonNull<u8>) -> Self {
        Self(val.as_ptr() as _)
    }
}

impl From<*const u8> for VirtAddr {
    #[inline(always)]
    fn from(val: *const u8) -> Self {
        Self(val as _)
    }
}

#[cfg(target_pointer_width = "64")]
impl From<u64> for PhysAddr {
    #[inline(always)]
    fn from(value: u64) -> Self {
        Self(value as _)
    }
}

#[cfg(target_pointer_width = "32")]
impl From<u32> for PhysAddr {
    #[inline(always)]
    fn from(value: u32) -> Self {
        Self(value as _)
    }
}

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

impl core::fmt::LowerHex for VirtAddr {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:#x}", self.raw())
    }
}

impl core::fmt::LowerHex for PhysAddr {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:#x}", self.raw())
    }
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
                    vaddr.raw(),
                    existing_paddr.raw()
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
