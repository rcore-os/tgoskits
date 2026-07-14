//! Device address and access width definitions.

use alloc::string::String;
use core::fmt::{Debug, LowerHex};

use ax_memory_addr::AddrRange;
use axvm_types::GuestPhysAddr;
pub use axvm_types::{AccessWidth, Port, SysRegAddr};

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
#[derive(Debug, Clone, Eq, PartialEq, thiserror::Error)]
pub enum DeviceError {
    /// No device found at the requested address.
    #[error("no device was found for the requested bus access")]
    NotFound,
    /// The access width does not match what the register expects.
    #[error("invalid device access width: expected {expected:?}, got {actual:?}")]
    InvalidWidth {
        /// The width the register expects.
        expected: AccessWidth,
        /// The width that was used.
        actual: AccessWidth,
    },
    /// Attempted to write to a read-only register.
    #[error("attempted to write a read-only device register")]
    ReadOnly,
    /// Attempted to read from a write-only register.
    #[error("attempted to read a write-only device register")]
    WriteOnly,
    /// The address is outside the device's range.
    #[error("device address {addr:#x} is outside the registered range")]
    OutOfRange {
        /// The address that was accessed.
        addr: u64,
    },
    /// The requested functionality is not yet implemented.
    #[error("device operation is not implemented")]
    Unimplemented,
    /// An internal error occurred in the device implementation.
    #[error("internal device error")]
    Internal,
    /// An operation received an invalid argument.
    #[error("invalid input for device operation {operation}: {detail}")]
    InvalidInput {
        /// The operation that rejected the input.
        operation: &'static str,
        /// Diagnostic detail describing the invalid input.
        detail: String,
    },
    /// Device data is malformed or inconsistent.
    #[error("invalid data for device operation {operation}: {detail}")]
    InvalidData {
        /// The operation that rejected the data.
        operation: &'static str,
        /// Diagnostic detail describing the malformed data.
        detail: String,
    },
    /// Device state does not allow the requested operation.
    #[error("invalid state for device operation {operation}: {detail}")]
    InvalidState {
        /// The operation that cannot run in the current state.
        operation: &'static str,
        /// Diagnostic detail describing the current state.
        detail: String,
    },
    /// The device does not support the requested operation.
    #[error("unsupported device operation {operation}: {detail}")]
    Unsupported {
        /// The unsupported operation.
        operation: &'static str,
        /// Diagnostic detail describing the limitation.
        detail: String,
    },
    /// A device allocation failed.
    #[error("out of memory during device operation {operation}")]
    OutOfMemory {
        /// The operation that attempted the allocation.
        operation: &'static str,
    },
    /// A device resource is currently busy.
    #[error("device resource {resource} is busy during {operation}")]
    ResourceBusy {
        /// The operation that attempted to use the resource.
        operation: &'static str,
        /// The busy resource.
        resource: String,
    },
    /// A device backend operation failed.
    #[error("device backend operation {operation} failed: {detail}")]
    Backend {
        /// The backend operation that failed.
        operation: &'static str,
        /// Diagnostic detail from the backend.
        detail: String,
    },
}

/// Result type returned by device access operations.
pub type DeviceResult<T = ()> = Result<T, DeviceError>;
