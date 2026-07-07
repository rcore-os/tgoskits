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

use core::fmt::{Debug, Formatter, LowerHex, UpperHex};

/// Result type returned by the OS-neutral AArch64 vCPU core.
pub type ArmVcpuResult<T = ()> = Result<T, ArmVcpuError>;

/// Errors produced by the OS-neutral AArch64 vCPU core.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArmVcpuError {
    /// A caller supplied an invalid argument or unsupported hardware encoding.
    InvalidInput,
    /// The requested operation is not supported by this CPU or this vCPU core.
    Unsupported,
    /// Hardware or software state is inconsistent with the requested transition.
    BadState,
}

/// Guest physical address.
#[derive(Clone, Copy, Eq, PartialEq, Ord, PartialOrd)]
pub struct ArmGuestPhysAddr(usize);

impl ArmGuestPhysAddr {
    /// Creates a guest physical address from a raw `usize`.
    pub const fn from_usize(addr: usize) -> Self {
        Self(addr)
    }

    /// Returns the raw address value.
    pub const fn as_usize(self) -> usize {
        self.0
    }
}

impl From<usize> for ArmGuestPhysAddr {
    fn from(value: usize) -> Self {
        Self::from_usize(value)
    }
}

impl From<ArmGuestPhysAddr> for usize {
    fn from(value: ArmGuestPhysAddr) -> Self {
        value.as_usize()
    }
}

impl Debug for ArmGuestPhysAddr {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        write!(f, "GPA({:#x})", self.0)
    }
}

impl LowerHex for ArmGuestPhysAddr {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:#x}", self.0)
    }
}

impl UpperHex for ArmGuestPhysAddr {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:#X}", self.0)
    }
}

/// AArch64 system-register address encoding used by trapped MRS/MSR exits.
#[derive(Clone, Copy, Eq, PartialEq, Ord, PartialOrd)]
pub struct ArmSysRegAddr(usize);

impl ArmSysRegAddr {
    /// Creates a system-register address from the ISS-derived encoding.
    pub const fn new(addr: usize) -> Self {
        Self(addr)
    }

    /// Returns the raw register address encoding.
    pub const fn addr(self) -> usize {
        self.0
    }
}

impl From<usize> for ArmSysRegAddr {
    fn from(value: usize) -> Self {
        Self::new(value)
    }
}

impl From<ArmSysRegAddr> for usize {
    fn from(value: ArmSysRegAddr) -> Self {
        value.addr()
    }
}

impl Debug for ArmSysRegAddr {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        write!(f, "ArmSysRegAddr({:#x})", self.0)
    }
}

impl LowerHex for ArmSysRegAddr {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:#x}", self.0)
    }
}

impl UpperHex for ArmSysRegAddr {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:#X}", self.0)
    }
}

/// Width of a trapped guest memory access.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum ArmAccessWidth {
    /// 8-bit access.
    Byte,
    /// 16-bit access.
    Word,
    /// 32-bit access.
    Dword,
    /// 64-bit access.
    Qword,
}

impl ArmAccessWidth {
    /// Returns this access width in bytes.
    pub const fn size(self) -> usize {
        match self {
            Self::Byte => 1,
            Self::Word => 2,
            Self::Dword => 4,
            Self::Qword => 8,
        }
    }
}

impl TryFrom<usize> for ArmAccessWidth {
    type Error = ArmVcpuError;

    fn try_from(value: usize) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::Byte),
            2 => Ok(Self::Word),
            4 => Ok(Self::Dword),
            8 => Ok(Self::Qword),
            _ => Err(ArmVcpuError::InvalidInput),
        }
    }
}

impl From<ArmAccessWidth> for usize {
    fn from(value: ArmAccessWidth) -> Self {
        value.size()
    }
}

/// Stage-2 page table configuration selected by the embedding VMM.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ArmNestedPagingConfig {
    /// Root physical address of the stage-2 page table.
    pub root_paddr: usize,
    /// Number of stage-2 page-table levels.
    pub levels: usize,
    /// Guest physical address width in bits.
    pub gpa_bits: usize,
    /// Hardware-specific mode value. For AArch64 this carries host PA bits when non-zero.
    pub mode: usize,
}

impl ArmNestedPagingConfig {
    /// Creates a nested paging configuration.
    pub const fn new(root_paddr: usize, levels: usize, gpa_bits: usize, mode: usize) -> Self {
        Self {
            root_paddr,
            levels,
            gpa_bits,
            mode,
        }
    }
}

/// VM-exit reason returned by the AArch64 vCPU core.
#[non_exhaustive]
#[derive(Debug)]
pub enum ArmVmExit {
    /// A guest instruction triggered a hypercall.
    Hypercall {
        /// Hypercall number.
        nr: u64,
        /// Hypercall arguments.
        args: [u64; 6],
    },
    /// The guest performed an MMIO read.
    MmioRead {
        /// Guest physical address being read.
        addr: ArmGuestPhysAddr,
        /// Access width.
        width: ArmAccessWidth,
        /// Destination guest register.
        reg: usize,
        /// Destination register width.
        reg_width: ArmAccessWidth,
        /// Whether the value should be sign-extended.
        signed_ext: bool,
    },
    /// The guest performed an MMIO write.
    MmioWrite {
        /// Guest physical address being written.
        addr: ArmGuestPhysAddr,
        /// Access width.
        width: ArmAccessWidth,
        /// Value written by the guest.
        data: u64,
    },
    /// The guest performed a system-register read.
    SysRegRead {
        /// System-register address.
        addr: ArmSysRegAddr,
        /// Destination guest register.
        reg: usize,
    },
    /// The guest performed a system-register write.
    SysRegWrite {
        /// System-register address.
        addr: ArmSysRegAddr,
        /// Value written by the guest.
        value: u64,
    },
    /// A physical host interrupt should be handled by the embedding VMM.
    ExternalInterrupt {
        /// Host or placeholder vector reported by the host adapter.
        vector: u64,
    },
    /// A guest PSCI CPU_OFF call was trapped.
    CpuDown {
        /// Guest-provided target state.
        state: u64,
    },
    /// A guest PSCI CPU_ON call was trapped.
    CpuUp {
        /// Target CPU affinity.
        target_cpu: u64,
        /// Guest entry point for the target CPU.
        entry_point: ArmGuestPhysAddr,
        /// Guest argument for the target CPU.
        arg: u64,
    },
    /// The guest requested system power-off.
    SystemDown,
    /// The guest wrote a GIC SGI system register.
    SendIPI {
        /// Primary target selector.
        target_cpu: u64,
        /// Auxiliary target selector.
        target_cpu_aux: u64,
        /// Whether the SGI targets all other vCPUs.
        send_to_all: bool,
        /// Whether the SGI targets the current vCPU.
        send_to_self: bool,
        /// SGI interrupt ID.
        vector: u64,
    },
    /// The vCPU handled the event internally.
    Nothing,
}
