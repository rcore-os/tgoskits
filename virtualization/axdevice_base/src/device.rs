//! Device address and access width definitions.

use core::fmt::{Debug, LowerHex, UpperHex};

use ax_memory_addr::AddrRange;
use axvm_types::GuestPhysAddr;

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
pub struct SysRegAddr(pub usize);

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

/// An address-like type that can be used to access devices.
pub trait DeviceAddr: Copy + Eq + Ord + core::fmt::Debug {}

/// A range of device addresses. It may be contiguous or not.
pub trait DeviceAddrRange {
    /// The address type of the range.
    type Addr: DeviceAddr;

    /// Returns whether the address range contains the given address.
    fn contains(&self, addr: Self::Addr) -> bool;
}

impl DeviceAddr for GuestPhysAddr {}

impl DeviceAddrRange for AddrRange<GuestPhysAddr> {
    type Addr = GuestPhysAddr;

    fn contains(&self, addr: Self::Addr) -> bool {
        Self::contains(*self, addr)
    }
}

impl DeviceAddr for SysRegAddr {}

/// A inclusive range of system register addresses.
///
/// Unlike [`AddrRange`], this type is inclusive on both ends.
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub struct SysRegAddrRange {
    /// The start address of the range.
    pub start: SysRegAddr,
    /// The end address of the range.
    pub end: SysRegAddr,
}

impl SysRegAddrRange {
    /// Creates a new [`SysRegAddrRange`] instance.
    pub fn new(start: SysRegAddr, end: SysRegAddr) -> Self {
        Self { start, end }
    }
}

impl DeviceAddrRange for SysRegAddrRange {
    type Addr = SysRegAddr;

    fn contains(&self, addr: Self::Addr) -> bool {
        addr.0 >= self.start.0 && addr.0 <= self.end.0
    }
}

impl LowerHex for SysRegAddrRange {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:#x}..={:#x}", self.start.0, self.end.0)
    }
}

impl DeviceAddr for Port {}

/// A inclusive range of port numbers.
///
/// Unlike [`AddrRange`], this type is inclusive on both ends.
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub struct PortRange {
    /// The start port number of the range.
    pub start: Port,
    /// The end port number of the range.
    pub end: Port,
}

impl PortRange {
    /// Creates a new [`PortRange`] instance.
    pub fn new(start: Port, end: Port) -> Self {
        Self { start, end }
    }
}

impl DeviceAddrRange for PortRange {
    type Addr = Port;

    fn contains(&self, addr: Self::Addr) -> bool {
        addr.0 >= self.start.0 && addr.0 <= self.end.0
    }
}

impl LowerHex for PortRange {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:#x}..={:#x}", self.start.0, self.end.0)
    }
}
