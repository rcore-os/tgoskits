// Copyright 2026 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use alloc::boxed::Box;
use core::{
    fmt::{Debug, Formatter, LowerHex, UpperHex},
    ops::Range,
};

/// Result type returned by the OS-neutral x86 vLAPIC devices.
pub type X86VlapicResult<T = ()> = Result<T, X86VlapicError>;

/// Timer callback type accepted by the embedding host.
pub type X86TimerCallback = Box<dyn FnOnce(u64) + Send + 'static>;

/// VM identifier used by x86 interrupt-controller emulation.
pub type X86VmId = usize;

/// vCPU identifier used by x86 interrupt-controller emulation.
pub type X86VcpuId = usize;

/// Guest interrupt vector used by x86 interrupt-controller emulation.
pub type X86InterruptVector = u8;

/// Errors produced by the OS-neutral x86 vLAPIC devices.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum X86VlapicError {
    /// A caller supplied an invalid argument or unsupported hardware encoding.
    InvalidInput,
    /// Device register contents could not be decoded as a valid hardware value.
    InvalidData,
    /// The requested operation is not supported by this device model.
    Unsupported,
    /// A host memory allocation failed.
    NoMemory,
    /// Device state is inconsistent with the requested transition.
    BadState,
}

macro_rules! define_addr_type {
    ($name:ident, $debug_prefix:literal) => {
        #[derive(Clone, Copy, Eq, PartialEq, Ord, PartialOrd)]
        pub struct $name(usize);

        impl $name {
            /// Creates an address from a raw `usize`.
            pub const fn from_usize(addr: usize) -> Self {
                Self(addr)
            }

            /// Returns the raw address value.
            pub const fn as_usize(self) -> usize {
                self.0
            }

            /// Returns this address as an immutable pointer.
            pub const fn as_ptr<T>(self) -> *const T {
                self.0 as *const T
            }

            /// Returns this address as a mutable pointer.
            pub const fn as_mut_ptr<T>(self) -> *mut T {
                self.0 as *mut T
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
                write!(f, "{}({:#x})", $debug_prefix, self.0)
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

define_addr_type!(X86GuestPhysAddr, "GPA");
define_addr_type!(X86HostPhysAddr, "HPA");
define_addr_type!(X86HostVirtAddr, "HVA");

/// x86 MSR address.
#[derive(Clone, Copy, Eq, PartialEq, Ord, PartialOrd)]
pub struct X86MsrAddr(usize);

impl X86MsrAddr {
    /// Creates an MSR address from the raw MSR number.
    pub const fn new(addr: usize) -> Self {
        Self(addr)
    }

    /// Returns the raw MSR number.
    pub const fn addr(self) -> usize {
        self.0
    }
}

impl From<usize> for X86MsrAddr {
    fn from(value: usize) -> Self {
        Self::new(value)
    }
}

impl Debug for X86MsrAddr {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        write!(f, "MSR({:#x})", self.0)
    }
}

impl LowerHex for X86MsrAddr {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:#x}", self.0)
    }
}

impl UpperHex for X86MsrAddr {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:#X}", self.0)
    }
}

/// The port number of an x86 I/O operation.
#[derive(Clone, Copy, Eq, PartialEq, Ord, PartialOrd)]
pub struct X86Port(u16);

impl X86Port {
    /// Creates a new x86 I/O port.
    pub const fn new(port: u16) -> Self {
        Self(port)
    }

    /// Returns the raw port number.
    pub const fn number(self) -> u16 {
        self.0
    }
}

impl From<u16> for X86Port {
    fn from(value: u16) -> Self {
        Self::new(value)
    }
}

impl Debug for X86Port {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        write!(f, "Port({:#x})", self.0)
    }
}

impl LowerHex for X86Port {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:#x}", self.0)
    }
}

impl UpperHex for X86Port {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:#X}", self.0)
    }
}

/// Width of a guest bus access.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum X86AccessWidth {
    /// 8-bit access.
    Byte,
    /// 16-bit access.
    Word,
    /// 32-bit access.
    Dword,
    /// 64-bit access.
    Qword,
}

impl X86AccessWidth {
    /// Returns this access width in bytes.
    pub const fn size(self) -> usize {
        match self {
            Self::Byte => 1,
            Self::Word => 2,
            Self::Dword => 4,
            Self::Qword => 8,
        }
    }

    /// Returns the bit range covered by this access.
    pub fn bits_range(self) -> Range<usize> {
        match self {
            Self::Byte => 0..8,
            Self::Word => 0..16,
            Self::Dword => 0..32,
            Self::Qword => 0..64,
        }
    }
}

impl TryFrom<usize> for X86AccessWidth {
    type Error = X86VlapicError;

    fn try_from(value: usize) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::Byte),
            2 => Ok(Self::Word),
            4 => Ok(Self::Dword),
            8 => Ok(Self::Qword),
            _ => Err(X86VlapicError::InvalidInput),
        }
    }
}

impl From<X86AccessWidth> for usize {
    fn from(value: X86AccessWidth) -> Self {
        value.size()
    }
}

/// A half-open range of guest physical addresses.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct X86GuestPhysAddrRange {
    /// Inclusive start address.
    pub start: X86GuestPhysAddr,
    /// Exclusive end address.
    pub end: X86GuestPhysAddr,
}

impl X86GuestPhysAddrRange {
    /// Creates a half-open address range.
    pub fn new(start: X86GuestPhysAddr, end: X86GuestPhysAddr) -> Self {
        assert!(start <= end, "invalid x86 guest physical address range");
        Self { start, end }
    }

    /// Returns whether the range contains `addr`.
    pub fn contains(self, addr: X86GuestPhysAddr) -> bool {
        self.start <= addr && addr < self.end
    }
}

/// An inclusive range of x86 I/O ports.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct X86PortRange {
    /// Inclusive start port.
    pub start: X86Port,
    /// Inclusive end port.
    pub end: X86Port,
}

impl X86PortRange {
    /// Creates an inclusive port range.
    pub const fn new(start: X86Port, end: X86Port) -> Self {
        Self { start, end }
    }

    /// Returns whether the range contains `port`.
    pub const fn contains(self, port: X86Port) -> bool {
        self.start.0 <= port.0 && port.0 <= self.end.0
    }
}

/// An inclusive range of x86 MSR addresses.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct X86MsrAddrRange {
    /// Inclusive start MSR.
    pub start: X86MsrAddr,
    /// Inclusive end MSR.
    pub end: X86MsrAddr,
}

impl X86MsrAddrRange {
    /// Creates an inclusive MSR range.
    pub const fn new(start: X86MsrAddr, end: X86MsrAddr) -> Self {
        Self { start, end }
    }

    /// Returns whether the range contains `addr`.
    pub const fn contains(self, addr: X86MsrAddr) -> bool {
        self.start.0 <= addr.0 && addr.0 <= self.end.0
    }
}
