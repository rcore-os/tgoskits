use core::fmt::{Debug, Formatter, LowerHex, UpperHex};

pub type LoongArchVcpuResult<T = ()> = Result<T, LoongArchVcpuError>;

/// Errors reported by the OS-neutral LoongArch vCPU core.
///
/// Kept in the same `thiserror` form as the Arm and RISC-V cores so callers
/// can fold it into [`crate::AxVmError::vcpu`] without a manual `Display`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum LoongArchVcpuError {
    /// Caller supplied an invalid value for a LoongArch vCPU operation.
    #[error("invalid LoongArch vCPU input")]
    InvalidInput,
    /// The requested operation is not supported by this backend.
    #[error("unsupported LoongArch vCPU operation")]
    Unsupported,
    /// The vCPU state does not allow the requested operation.
    #[error("invalid LoongArch vCPU state")]
    BadState,
    /// The host timer could not be registered or cancelled.
    #[error("LoongArch guest timer is unavailable")]
    TimerUnavailable,
}

macro_rules! define_addr_type {
    ($name:ident, $label:literal) => {
        #[repr(transparent)]
        #[derive(Clone, Copy, Default, Eq, PartialEq, Ord, PartialOrd)]
        pub struct $name(usize);

        impl $name {
            pub const fn from_usize(addr: usize) -> Self {
                Self(addr)
            }

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

        impl Debug for $name {
            fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
                write!(f, "{}({:#x})", $label, self.0)
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

define_addr_type!(LoongArchGuestPhysAddr, "LoongArchGPA");
define_addr_type!(LoongArchGuestVirtAddr, "LoongArchGVA");
define_addr_type!(LoongArchHostPhysAddr, "LoongArchHPA");
define_addr_type!(LoongArchHostVirtAddr, "LoongArchHVA");

pub type LoongArchVmId = usize;
pub type LoongArchVcpuId = usize;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum LoongArchAccessWidth {
    Byte,
    Word,
    Dword,
    Qword,
}

impl LoongArchAccessWidth {
    pub const fn size(self) -> usize {
        match self {
            Self::Byte => 1,
            Self::Word => 2,
            Self::Dword => 4,
            Self::Qword => 8,
        }
    }
}

impl TryFrom<usize> for LoongArchAccessWidth {
    type Error = LoongArchVcpuError;

    fn try_from(value: usize) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::Byte),
            2 => Ok(Self::Word),
            4 => Ok(Self::Dword),
            8 => Ok(Self::Qword),
            _ => Err(LoongArchVcpuError::InvalidInput),
        }
    }
}

impl From<LoongArchAccessWidth> for usize {
    fn from(value: LoongArchAccessWidth) -> Self {
        value.size()
    }
}

bitflags::bitflags! {
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub struct LoongArchAccessFlags: usize {
        const READ = 1 << 0;
        const WRITE = 1 << 1;
        const EXECUTE = 1 << 2;
        const USER = 1 << 3;
        const DEVICE = 1 << 4;
        const UNCACHED = 1 << 5;
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LoongArchNestedPagingConfig {
    pub root_paddr: LoongArchHostPhysAddr,
    pub levels: usize,
    pub gpa_bits: usize,
    pub mode: usize,
}

/// Host CPU-local operand state for one LVZ exit, resolved while pinned.
///
/// `capture_exit` runs while the vCPU is bound to one host CPU, but `finish_exit`
/// interprets the exit later in plain task context, which may be a different
/// CPU. Every native CPU-local side effect of the exit (`cpucfg` reads and the
/// unhandled IOCSR passthrough reads and writes) is therefore resolved at
/// capture time and carried here, so task-stage interpretation never reads or
/// writes a foreign CPU's local CSR bank.
///
/// Only the operands the faulting instruction actually needs are captured, never
/// the whole register file.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum LoongArchPinnedHost {
    /// This exit needs no pinned host CPU state: hypercall, translation fault,
    /// CSR or timer access, or any other purely software emulation.
    #[default]
    None,
    /// A `cpucfg` read resolved on the pinned CPU. `index` is the guest-selected
    /// CPUCFG index and `value` is the fully resolved guest-visible word.
    Cpucfg { index: usize, value: usize },
    /// An IOCSR read resolved on the pinned CPU. `value` is the final load
    /// result, taken either from the guest-owned IOCSR state or from the host
    /// passthrough.
    IocsrRead { addr: usize, value: usize },
    /// An IOCSR write whose target is emulated by the guest-owned IOCSR model.
    /// The task stage applies it and must not touch host CPU-local state.
    IocsrWriteGuest,
    /// An IOCSR write whose target is not guest-owned. The raw host passthrough
    /// write was already issued on the pinned CPU, so the task stage must not
    /// repeat it on whatever CPU it migrated to.
    IocsrWritePassthrough { addr: usize },
}

impl LoongArchNestedPagingConfig {
    pub const fn new(root_paddr: usize, levels: usize, gpa_bits: usize, mode: usize) -> Self {
        Self {
            root_paddr: LoongArchHostPhysAddr::from_usize(root_paddr),
            levels,
            gpa_bits,
            mode,
        }
    }
}

#[derive(Debug)]
pub enum LoongArchVmExit {
    Machine(ax_cpu::virtualization::Exit),
    Hypercall {
        nr: u64,
        args: [u64; 6],
    },
    MmioRead {
        addr: LoongArchGuestPhysAddr,
        width: LoongArchAccessWidth,
        reg: usize,
        reg_width: LoongArchAccessWidth,
        signed_ext: bool,
    },
    MmioWrite {
        addr: LoongArchGuestPhysAddr,
        width: LoongArchAccessWidth,
        data: u64,
    },
    NestedPageFault {
        addr: LoongArchGuestPhysAddr,
        access_flags: LoongArchAccessFlags,
    },

    Idle,
    Halt,
    Nothing,
}
