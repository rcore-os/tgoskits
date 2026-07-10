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

use core::{
    fmt::{Debug, Formatter, LowerHex, UpperHex},
    ops::{Add, AddAssign},
};

use bitflags::bitflags;

/// Size of a 4 KiB page.
pub const X86_PAGE_SIZE_4K: usize = 0x1000;

/// Result type returned by the OS-neutral x86 vCPU core.
pub type X86VcpuResult<T = ()> = Result<T, X86VcpuError>;

/// Errors produced by the OS-neutral x86 vCPU core.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum X86VcpuError {
    /// A caller supplied an invalid argument or unsupported hardware encoding.
    InvalidInput,
    /// Hardware register or exit data could not be decoded as a valid value.
    InvalidData,
    /// The requested operation is not supported by this CPU or vCPU backend.
    Unsupported,
    /// Hardware or software state is inconsistent with the requested transition.
    BadState,
    /// A host allocation failed.
    NoMemory,
    /// The requested hardware resource is already in use.
    ResourceBusy,
}

impl From<x86_vlapic::X86VlapicError> for X86VcpuError {
    fn from(err: x86_vlapic::X86VlapicError) -> Self {
        match err {
            x86_vlapic::X86VlapicError::InvalidInput => Self::InvalidInput,
            x86_vlapic::X86VlapicError::InvalidData => Self::InvalidData,
            x86_vlapic::X86VlapicError::Unsupported => Self::Unsupported,
            x86_vlapic::X86VlapicError::NoMemory => Self::NoMemory,
            x86_vlapic::X86VlapicError::BadState => Self::BadState,
        }
    }
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

        impl Add<usize> for $name {
            type Output = Self;

            fn add(self, rhs: usize) -> Self::Output {
                Self(self.0 + rhs)
            }
        }

        impl AddAssign<usize> for $name {
            fn add_assign(&mut self, rhs: usize) {
                self.0 += rhs;
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
define_addr_type!(X86GuestVirtAddr, "GVA");
define_addr_type!(X86HostPhysAddr, "HPA");
define_addr_type!(X86HostVirtAddr, "HVA");

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
        write!(f, "X86Port({:#x})", self.0)
    }
}

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

/// Width of a trapped guest bus access.
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
    pub fn bits_range(self) -> core::ops::Range<usize> {
        match self {
            Self::Byte => 0..8,
            Self::Word => 0..16,
            Self::Dword => 0..32,
            Self::Qword => 0..64,
        }
    }
}

impl TryFrom<usize> for X86AccessWidth {
    type Error = X86VcpuError;

    fn try_from(value: usize) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::Byte),
            2 => Ok(Self::Word),
            4 => Ok(Self::Dword),
            8 => Ok(Self::Qword),
            _ => Err(X86VcpuError::InvalidInput),
        }
    }
}

impl From<X86AccessWidth> for usize {
    fn from(value: X86AccessWidth) -> Self {
        value.size()
    }
}

bitflags! {
    /// Access flags reported for a nested page fault.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub struct X86AccessFlags: usize {
        /// Read access.
        const READ = 1 << 0;
        /// Write access.
        const WRITE = 1 << 1;
        /// Execute access.
        const EXECUTE = 1 << 2;
    }
}

/// Information about a nested guest page-table fault.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct X86NestedPageFaultInfo {
    /// Faulting guest physical address.
    pub fault_guest_paddr: X86GuestPhysAddr,
    /// Fault access flags.
    pub access_flags: X86AccessFlags,
}

/// Nested page table configuration selected by the embedding VMM.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct X86NestedPagingConfig {
    /// Root physical address of the nested page table.
    pub root_paddr: X86HostPhysAddr,
    /// Number of nested page-table levels.
    pub levels: usize,
    /// Guest physical address width in bits.
    pub gpa_bits: usize,
    /// Hardware-specific mode value.
    pub mode: usize,
}

impl X86NestedPagingConfig {
    /// Creates a nested paging configuration.
    pub const fn new(
        root_paddr: X86HostPhysAddr,
        levels: usize,
        gpa_bits: usize,
        mode: usize,
    ) -> Self {
        Self {
            root_paddr,
            levels,
            gpa_bits,
            mode,
        }
    }
}

/// VM-exit reason returned by the x86 vCPU core.
#[derive(Debug)]
#[non_exhaustive]
pub enum X86VmExit {
    /// A guest instruction triggered a hypercall.
    Hypercall {
        /// Hypercall number.
        nr: u64,
        /// Hypercall arguments.
        args: [u64; 6],
    },
    /// The guest performed a port I/O read.
    PortIoRead {
        /// I/O port.
        port: X86Port,
        /// Access width.
        width: X86AccessWidth,
    },
    /// The guest performed a port I/O write.
    PortIoWrite {
        /// I/O port.
        port: X86Port,
        /// Access width.
        width: X86AccessWidth,
        /// Value written by the guest.
        data: u64,
    },
    /// The guest performed an MMIO read.
    MmioRead {
        /// Guest physical address.
        addr: X86GuestPhysAddr,
        /// Access width.
        width: X86AccessWidth,
        /// Destination guest register.
        reg: usize,
        /// Destination register width.
        reg_width: X86AccessWidth,
        /// Whether the value should be sign-extended.
        signed_ext: bool,
    },
    /// The guest performed an MMIO write.
    MmioWrite {
        /// Guest physical address.
        addr: X86GuestPhysAddr,
        /// Access width.
        width: X86AccessWidth,
        /// Value written by the guest.
        data: u64,
    },
    /// The guest performed an MSR read.
    MsrRead {
        /// MSR address.
        addr: X86MsrAddr,
    },
    /// The guest performed an MSR write.
    MsrWrite {
        /// MSR address.
        addr: X86MsrAddr,
        /// Value written by the guest.
        value: u64,
    },
    /// A nested page fault occurred.
    NestedPageFault {
        /// Faulting guest physical address.
        addr: X86GuestPhysAddr,
        /// Access flags.
        access_flags: X86AccessFlags,
    },
    /// A physical host interrupt should be handled by the embedding VMM.
    ExternalInterrupt {
        /// Host vector reported by the backend.
        vector: u8,
    },
    /// The preemption timer expired or the backend wants the VMM to poll timers.
    PreemptionTimer,
    /// A guest EOI completed.
    InterruptEnd {
        /// Vector that may require IOAPIC EOI propagation.
        vector: Option<u8>,
    },
    /// The guest halted.
    Halt,
    /// The guest requested system power-off.
    SystemDown,
    /// VM entry failed in hardware.
    FailEntry {
        /// Hardware entry-failure reason.
        hardware_entry_failure_reason: usize,
    },
    /// The exit was handled inside the x86 core.
    Nothing,
}
