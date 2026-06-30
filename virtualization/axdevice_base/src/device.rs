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
pub trait DeviceAddrRange: Copy + Eq + LowerHex {
    /// The address type of the range.
    type Addr: DeviceAddr;

    /// The name of the device bus that uses this range type.
    const BUS_NAME: &'static str;

    /// Returns whether the address range contains the given address.
    fn contains(&self, addr: Self::Addr) -> bool;

    /// Returns whether the address range is empty or invalid.
    fn is_empty(&self) -> bool;

    /// Returns whether this address range overlaps another range.
    fn overlaps(&self, other: &Self) -> bool;
}

impl DeviceAddr for GuestPhysAddr {}

impl DeviceAddrRange for AddrRange<GuestPhysAddr> {
    type Addr = GuestPhysAddr;

    const BUS_NAME: &'static str = "mmio";

    fn contains(&self, addr: Self::Addr) -> bool {
        Self::contains(*self, addr)
    }

    fn is_empty(&self) -> bool {
        Self::is_empty(*self)
    }

    fn overlaps(&self, other: &Self) -> bool {
        Self::overlaps(*self, *other)
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

    const BUS_NAME: &'static str = "sys_reg";

    fn contains(&self, addr: Self::Addr) -> bool {
        addr.0 >= self.start.0 && addr.0 <= self.end.0
    }

    fn is_empty(&self) -> bool {
        self.start > self.end
    }

    fn overlaps(&self, other: &Self) -> bool {
        !self.is_empty() && !other.is_empty() && self.start <= other.end && other.start <= self.end
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

    const BUS_NAME: &'static str = "port";

    fn contains(&self, addr: Self::Addr) -> bool {
        addr.0 >= self.start.0 && addr.0 <= self.end.0
    }

    fn is_empty(&self) -> bool {
        self.start > self.end
    }

    fn overlaps(&self, other: &Self) -> bool {
        !self.is_empty() && !other.is_empty() && self.start <= other.end && other.start <= self.end
    }
}

impl LowerHex for PortRange {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:#x}..={:#x}", self.start.0, self.end.0)
    }
}

// ---------------------------------------------------------------------------
// Unified bus-access types
// ---------------------------------------------------------------------------

/// The kind of bus a device is connected to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BusKind {
    /// Memory-mapped I/O bus.
    Mmio,
    /// Port I/O bus (x86 only).
    Port,
    /// System register bus (ARM only).
    SysReg,
}

/// An access issued by a vCPU to a device on a bus.
#[derive(Debug, Clone, Copy)]
pub struct BusAccess {
    /// The kind of bus being accessed.
    pub kind: BusKind,
    /// `true` if this is a read access; `false` for write.
    pub is_read: bool,
    /// The address being accessed.
    pub addr: u64,
    /// The width of the access.
    pub width: AccessWidth,
    /// The data to write (ignored for reads).
    pub data: u64,
}

/// The result of a bus access dispatched to a device.
#[derive(Debug, Clone, Copy)]
pub enum BusResponse {
    /// A read response with the value.
    Read {
        /// The value read from the device.
        value: u64,
    },
    /// A write acknowledgment.
    Write,
}

/// Errors that can occur during device access handling.
#[derive(Debug, Clone)]
pub enum DeviceError {
    /// No device found at the requested address.
    NotFound,
    /// The access width does not match what the register expects.
    InvalidWidth {
        /// The width the register expects.
        expected: AccessWidth,
        /// The width that was used.
        actual: AccessWidth,
    },
    /// Attempted to write to a read-only register.
    ReadOnly,
    /// Attempted to read from a write-only register.
    WriteOnly,
    /// The address is outside the device's range.
    OutOfRange {
        /// The address that was accessed.
        addr: u64,
    },
    /// The requested functionality is not yet implemented.
    Unimplemented,
    /// An internal error occurred in the device implementation.
    Internal,
}
