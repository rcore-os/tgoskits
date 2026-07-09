//! OS-neutral value types exposed by the RISC-V vCPU core.

use core::fmt::{Display, Formatter};

/// RISC-V vCPU result type.
pub type RiscvVcpuResult<T = ()> = Result<T, RiscvVcpuError>;

/// Errors reported by the RISC-V vCPU core.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RiscvVcpuError {
    /// Caller supplied an invalid value.
    InvalidInput,
    /// The requested operation is not supported by this backend.
    Unsupported,
    /// The vCPU state does not allow the requested operation.
    BadState,
    /// Hardware or emulation state contained an invalid trap.
    InvalidTrap,
    /// Guest instruction decoding failed.
    DecodeFailed,
    /// Guest memory access failed while emulating an instruction.
    GuestMemoryFault,
}

impl Display for RiscvVcpuError {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        let msg = match self {
            Self::InvalidInput => "invalid RISC-V vCPU input",
            Self::Unsupported => "unsupported RISC-V vCPU operation",
            Self::BadState => "invalid RISC-V vCPU state",
            Self::InvalidTrap => "invalid RISC-V trap state",
            Self::DecodeFailed => "failed to decode guest instruction",
            Self::GuestMemoryFault => "guest memory access failed",
        };
        f.write_str(msg)
    }
}

impl core::error::Error for RiscvVcpuError {}

macro_rules! riscv_addr_type {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[repr(transparent)]
        #[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
        pub struct $name(usize);

        impl $name {
            /// Creates an address from a raw `usize`.
            pub const fn from_usize(value: usize) -> Self {
                Self(value)
            }

            /// Returns the raw address value.
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
    };
}

riscv_addr_type! {
    /// Guest physical address.
    RiscvGuestPhysAddr
}

riscv_addr_type! {
    /// Guest virtual address.
    RiscvGuestVirtAddr
}

riscv_addr_type! {
    /// Host physical address.
    RiscvHostPhysAddr
}

riscv_addr_type! {
    /// Host virtual address.
    RiscvHostVirtAddr
}

impl<T> From<*const T> for RiscvHostVirtAddr {
    fn from(ptr: *const T) -> Self {
        Self::from_usize(ptr as usize)
    }
}

impl<T> From<*mut T> for RiscvHostVirtAddr {
    fn from(ptr: *mut T) -> Self {
        Self::from_usize(ptr as usize)
    }
}

/// Virtual machine identifier.
pub type RiscvVmId = usize;

/// Virtual CPU identifier within a VM.
pub type RiscvVcpuId = usize;

/// The width of a guest memory access.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RiscvAccessWidth {
    /// 8-bit access.
    Byte,
    /// 16-bit access.
    Word,
    /// 32-bit access.
    Dword,
    /// 64-bit access.
    Qword,
}

impl RiscvAccessWidth {
    /// Returns the access size in bytes.
    pub const fn size(self) -> usize {
        match self {
            Self::Byte => 1,
            Self::Word => 2,
            Self::Dword => 4,
            Self::Qword => 8,
        }
    }
}

impl TryFrom<usize> for RiscvAccessWidth {
    type Error = RiscvVcpuError;

    fn try_from(value: usize) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::Byte),
            2 => Ok(Self::Word),
            4 => Ok(Self::Dword),
            8 => Ok(Self::Qword),
            _ => Err(RiscvVcpuError::InvalidInput),
        }
    }
}

impl From<RiscvAccessWidth> for usize {
    fn from(width: RiscvAccessWidth) -> Self {
        width.size()
    }
}

bitflags::bitflags! {
    /// Guest memory access flags.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub struct RiscvAccessFlags: usize {
        /// Read access.
        const READ = 1 << 0;
        /// Write access.
        const WRITE = 1 << 1;
        /// Execute access.
        const EXECUTE = 1 << 2;
        /// User-mode access.
        const USER = 1 << 3;
        /// Device memory access.
        const DEVICE = 1 << 4;
        /// Uncached memory access.
        const UNCACHED = 1 << 5;
    }
}

/// Nested paging configuration selected by the VMM.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RiscvNestedPagingConfig {
    /// Root physical address of the nested page table.
    pub root_paddr: RiscvHostPhysAddr,
    /// Number of guest-stage page-table levels.
    pub levels: usize,
    /// Guest physical address width in bits.
    pub gpa_bits: usize,
    /// Architecture-specific `hgatp.MODE` value.
    pub mode: usize,
}

impl RiscvNestedPagingConfig {
    /// Creates a nested paging configuration.
    pub const fn new(root_paddr: usize, levels: usize, gpa_bits: usize, mode: usize) -> Self {
        Self {
            root_paddr: RiscvHostPhysAddr::from_usize(root_paddr),
            levels,
            gpa_bits,
            mode,
        }
    }
}

/// VM exits returned by the RISC-V vCPU core.
#[derive(Debug)]
pub enum RiscvVmExit {
    /// Guest issued a hypercall.
    Hypercall {
        /// Hypercall number.
        nr: u64,
        /// Hypercall arguments.
        args: [u64; 6],
    },
    /// Guest MMIO read.
    MmioRead {
        /// Guest physical address.
        addr: RiscvGuestPhysAddr,
        /// Access width.
        width: RiscvAccessWidth,
        /// Destination register.
        reg: usize,
        /// Destination register width.
        reg_width: RiscvAccessWidth,
        /// Whether the read result should be sign-extended.
        signed_ext: bool,
    },
    /// Guest MMIO write.
    MmioWrite {
        /// Guest physical address.
        addr: RiscvGuestPhysAddr,
        /// Access width.
        width: RiscvAccessWidth,
        /// Written value.
        data: u64,
    },
    /// Guest-stage page fault that was not decoded as MMIO.
    NestedPageFault {
        /// Faulting guest physical address.
        addr: RiscvGuestPhysAddr,
        /// Fault access flags.
        access_flags: RiscvAccessFlags,
    },
    /// Host external interrupt while running the vCPU.
    ExternalInterrupt {
        /// Host interrupt vector.
        vector: u64,
    },
    /// Guest requested another CPU to start.
    CpuUp {
        /// Target vCPU or hart ID.
        target_cpu: u64,
        /// Guest entry point.
        entry_point: RiscvGuestPhysAddr,
        /// Guest argument.
        arg: u64,
    },
    /// Guest requested this CPU to stop.
    CpuDown {
        /// Guest CPU state value.
        state: u64,
    },
    /// Guest halted.
    Halt,
    /// Guest requested system shutdown.
    SystemDown,
    /// No host-visible action is needed.
    Nothing,
}
