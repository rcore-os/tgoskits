//! Definitions about device accessing.

use core::fmt::{Debug, LowerHex, UpperHex};

mod device_addr;

pub use device_addr::*;

/// The width of an access.
///
/// Note that the term "word" here refers to 16-bit data, as in the x86 architecture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AccessWidth {
    /// 8-bit access.
    Byte,
    /// 16-bit access.
    Word,
    /// 32-bit access.
    Dword,
    /// 64-bit access.
    Qword,
}

impl TryFrom<usize> for AccessWidth {
    type Error = ();

    fn try_from(value: usize) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::Byte),
            2 => Ok(Self::Word),
            4 => Ok(Self::Dword),
            8 => Ok(Self::Qword),
            _ => Err(()),
        }
    }
}

impl From<AccessWidth> for usize {
    fn from(width: AccessWidth) -> usize {
        match width {
            AccessWidth::Byte => 1,
            AccessWidth::Word => 2,
            AccessWidth::Dword => 4,
            AccessWidth::Qword => 8,
        }
    }
}

impl AccessWidth {
    /// Returns the size of the access in bytes.
    pub fn size(&self) -> usize {
        (*self).into()
    }

    /// Returns the range of bits that the access covers.
    pub fn bits_range(&self) -> core::ops::Range<usize> {
        match self {
            AccessWidth::Byte => 0..8,
            AccessWidth::Word => 0..16,
            AccessWidth::Dword => 0..32,
            AccessWidth::Qword => 0..64,
        }
    }
}

/// The port number of an I/O operation.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Port(pub u16);

impl Port {
    /// Creates a new `Port` instance.
    pub fn new(port: u16) -> Self {
        Self(port)
    }

    /// Returns the port number.
    pub fn number(&self) -> u16 {
        self.0
    }
}

impl LowerHex for Port {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Port({:#x})", self.0)
    }
}

impl UpperHex for Port {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Port({:#X})", self.0)
    }
}

impl Debug for Port {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Port({})", self.0)
    }
}

/// A system register address.
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd)]
pub struct SysRegAddr(pub usize); // u32 seems to be enough, but we use usize for generality.

impl SysRegAddr {
    /// Creates a new `SysRegAddr` instance.
    pub const fn new(addr: usize) -> Self {
        Self(addr)
    }

    /// Returns the address.
    pub const fn addr(&self) -> usize {
        self.0
    }
}

impl LowerHex for SysRegAddr {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "SysRegAddr({:#x})", self.0)
    }
}

impl UpperHex for SysRegAddr {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "SysRegAddr({:#X})", self.0)
    }
}

impl Debug for SysRegAddr {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "SysRegAddr({})", self.0)
    }
}
