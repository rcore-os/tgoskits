use core::fmt::LowerHex;

use memory_addr::AddrRange;

use crate::GuestPhysAddr;

use super::{Port, SysRegAddr};

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
