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

/// Guest virtual address captured from an AArch64 fault register.
///
/// This is intentionally distinct from [`ArmGuestPhysAddr`] so exception
/// injection cannot substitute an IPA for an architecturally invalid FAR.
#[derive(Clone, Copy, Eq, PartialEq, Ord, PartialOrd)]
pub struct ArmGuestVirtAddr(u64);

impl ArmGuestVirtAddr {
    /// Creates a guest virtual address from its architectural value.
    pub const fn from_u64(address: u64) -> Self {
        Self(address)
    }

    /// Returns the architectural address value.
    pub const fn as_u64(self) -> u64 {
        self.0
    }
}

impl From<u64> for ArmGuestVirtAddr {
    fn from(value: u64) -> Self {
        Self::from_u64(value)
    }
}

impl Debug for ArmGuestVirtAddr {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        write!(f, "GVA({:#x})", self.0)
    }
}

impl LowerHex for ArmGuestVirtAddr {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:#x}", self.0)
    }
}

impl UpperHex for ArmGuestVirtAddr {
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

/// A GICv3 common CPU-interface register trapped for VM-local emulation.
///
/// These registers share one architectural trap control on implementations
/// that do not provide the dedicated `ICC_DIR_EL1` trap. Keeping the register
/// identity typed prevents raw system-register encodings from leaking into the
/// VMM boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArmGicCpuInterfaceRegister {
    /// `ICC_CTLR_EL1`, the common CPU-interface control register.
    Control,
    /// `ICC_PMR_EL1`, the virtual priority-mask register.
    PriorityMask,
    /// `ICC_RPR_EL1`, the virtual running-priority register.
    RunningPriority,
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
    /// An SMC function not owned by the vCPU core requires VMM mediation.
    FirmwareCall {
        /// Valid 32-bit SMCCC function identifier.
        function: u32,
        /// First three function arguments.
        args: [u64; 3],
    },
    /// A guest data abort whose address ownership is intentionally unresolved.
    DataAbort {
        /// Architectural fault information captured by the vCPU core.
        abort: crate::ArmDataAbort,
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
    /// The guest read a trapped GICv3 common CPU-interface register.
    GicCpuInterfaceRead {
        /// Register selected by the trapped MRS instruction.
        register: ArmGicCpuInterfaceRegister,
        /// Destination guest general-purpose register.
        destination: usize,
    },
    /// The guest wrote a trapped GICv3 common CPU-interface register.
    GicCpuInterfaceWrite {
        /// Register selected by the trapped MSR instruction.
        register: ArmGicCpuInterfaceRegister,
        /// Value written by the guest.
        value: u64,
    },
    /// A physical host interrupt should be handled by the embedding VMM.
    ExternalInterrupt,
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
        /// Complete ICC_SGI1R_EL1 value, including affinity and range selector.
        value: u64,
    },
    /// The guest wrote ICC_DIR_EL1 while deactivation trapping was enabled.
    DeactivateInterrupt {
        /// Guest-visible INTID carried by ICC_DIR_EL1.
        intid: u32,
    },
    /// The vCPU handled the event internally.
    Nothing,
}
