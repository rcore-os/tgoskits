use core::fmt::{Debug, Formatter, LowerHex, UpperHex};

pub type LoongArchVcpuResult<T = ()> = Result<T, LoongArchVcpuError>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LoongArchVcpuError {
    InvalidInput,
    Unsupported,
    BadState,
}

macro_rules! define_addr_type {
    ($name:ident, $label:literal) => {
        #[repr(transparent)]
        #[derive(Clone, Copy, Default, Eq, PartialEq, Ord, PartialOrd)]
        pub struct $name(usize);

        impl $name {
            pub const fn from_usize(addr: usize) -> Self {
                Self(addr)
            }

            pub const fn as_usize(self) -> usize {
                self.0
            }
        }

        impl From<usize> for $name {
            fn from(value: usize) -> Self {
                Self::from_usize(value)
            }
        }

        impl From<$name> for usize {
            fn from(value: $name) -> Self {
                value.as_usize()
            }
        }

        impl Debug for $name {
            fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
                write!(f, "{}({:#x})", $label, self.0)
            }
        }

        impl LowerHex for $name {
            fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
                write!(f, "{:#x}", self.0)
            }
        }

        impl UpperHex for $name {
            fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
                write!(f, "{:#X}", self.0)
            }
        }
    };
}

define_addr_type!(LoongArchGuestPhysAddr, "LoongArchGPA");
define_addr_type!(LoongArchGuestVirtAddr, "LoongArchGVA");
define_addr_type!(LoongArchHostPhysAddr, "LoongArchHPA");
define_addr_type!(LoongArchHostVirtAddr, "LoongArchHVA");

pub type LoongArchVmId = usize;
pub type LoongArchVcpuId = usize;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum LoongArchAccessWidth {
    Byte,
    Word,
    Dword,
    Qword,
}

impl LoongArchAccessWidth {
    pub const fn size(self) -> usize {
        match self {
            Self::Byte => 1,
            Self::Word => 2,
            Self::Dword => 4,
            Self::Qword => 8,
        }
    }
}

impl TryFrom<usize> for LoongArchAccessWidth {
    type Error = LoongArchVcpuError;

    fn try_from(value: usize) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::Byte),
            2 => Ok(Self::Word),
            4 => Ok(Self::Dword),
            8 => Ok(Self::Qword),
            _ => Err(LoongArchVcpuError::InvalidInput),
        }
    }
}

impl From<LoongArchAccessWidth> for usize {
    fn from(value: LoongArchAccessWidth) -> Self {
        value.size()
    }
}

bitflags::bitflags! {
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub struct LoongArchAccessFlags: usize {
        const READ = 1 << 0;
        const WRITE = 1 << 1;
        const EXECUTE = 1 << 2;
        const USER = 1 << 3;
        const DEVICE = 1 << 4;
        const UNCACHED = 1 << 5;
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LoongArchNestedPagingConfig {
    pub root_paddr: LoongArchHostPhysAddr,
    pub levels: usize,
    pub gpa_bits: usize,
    pub mode: usize,
}

impl LoongArchNestedPagingConfig {
    pub const fn new(root_paddr: usize, levels: usize, gpa_bits: usize, mode: usize) -> Self {
        Self {
            root_paddr: LoongArchHostPhysAddr::from_usize(root_paddr),
            levels,
            gpa_bits,
            mode,
        }
    }
}

#[non_exhaustive]
#[derive(Debug)]
pub enum LoongArchVmExit {
    Hypercall {
        nr: u64,
        args: [u64; 6],
    },
    MmioRead {
        addr: LoongArchGuestPhysAddr,
        width: LoongArchAccessWidth,
        reg: usize,
        reg_width: LoongArchAccessWidth,
        signed_ext: bool,
    },
    MmioWrite {
        addr: LoongArchGuestPhysAddr,
        width: LoongArchAccessWidth,
        data: u64,
    },
    NestedPageFault {
        addr: LoongArchGuestPhysAddr,
        access_flags: LoongArchAccessFlags,
    },
    ExternalInterrupt {
        vector: u64,
    },
    Idle,
    Halt,
    Nothing,
}
