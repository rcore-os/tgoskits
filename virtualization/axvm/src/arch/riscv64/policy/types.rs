//! OS-neutral value types exposed by the RISC-V vCPU core.

use crate::architecture::ops::RegisterCompletion;

/// RISC-V vCPU result type.
pub type RiscvVcpuResult<T = ()> = Result<T, RiscvVcpuError>;

/// Errors reported by the RISC-V vCPU core.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum RiscvVcpuError {
    /// Caller supplied an invalid value.
    #[error("invalid RISC-V vCPU input")]
    InvalidInput,
    /// The requested operation is not supported by this backend.
    #[error("unsupported RISC-V vCPU operation")]
    Unsupported,
    /// The vCPU state does not allow the requested operation.
    #[error("invalid RISC-V vCPU state")]
    BadState,
    /// Hardware or emulation state contained an invalid trap.
    #[error("invalid RISC-V trap state")]
    InvalidTrap,
    /// Guest instruction decoding failed.
    #[error("failed to decode guest instruction")]
    DecodeFailed,
    /// Guest memory access failed while emulating an instruction.
    #[error("guest memory access failed")]
    GuestMemoryFault,
}

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

/// SBI calling convention used by an IPI request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RiscvIpiAbi {
    /// Legacy SBI `SEND_IPI` extension.
    Legacy,
    /// SBI v0.2 or newer IPI extension.
    SbiV02,
}

/// Result of routing an SBI IPI request through the VMM.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RiscvIpiCompletion {
    /// Every selected hart accepted the virtual software interrupt.
    Success,
    /// At least one selected hart was invalid or unavailable to the guest.
    InvalidParameter,
    /// Delivery failed after the request had been validated.
    Failed,
}

/// Decoded SBI IPI request forwarded to the VMM.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RiscvIpiRequest {
    hart_mask: usize,
    hart_mask_base: usize,
    abi: RiscvIpiAbi,
}

impl RiscvIpiRequest {
    pub(crate) const fn new(hart_mask: usize, hart_mask_base: usize, abi: RiscvIpiAbi) -> Self {
        Self {
            hart_mask,
            hart_mask_base,
            abi,
        }
    }

    /// Returns the SBI hart mask bits.
    pub const fn hart_mask(self) -> usize {
        self.hart_mask
    }

    /// Returns the SBI hart mask base.
    pub const fn hart_mask_base(self) -> usize {
        self.hart_mask_base
    }

    /// Returns the SBI calling convention that produced this request.
    pub const fn abi(self) -> RiscvIpiAbi {
        self.abi
    }
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

/// Owned SBI ABI values captured before backend retirement.
#[derive(Clone, Copy, Debug)]
pub struct RiscvSbiCall {
    pub extension: usize,
    pub function: usize,
    pub arguments: [usize; 6],
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
        /// Length in bytes of the emulated load instruction.
        advance: usize,
    },
    /// Guest MMIO write.
    MmioWrite {
        /// Guest physical address.
        addr: RiscvGuestPhysAddr,
        /// Access width.
        width: RiscvAccessWidth,
        /// Written value.
        data: u64,
        /// Length in bytes of the emulated store instruction.
        advance: usize,
    },
    /// Guest-stage page fault that was not decoded as MMIO.
    NestedPageFault {
        /// Faulting guest physical address.
        addr: RiscvGuestPhysAddr,
        /// Fault access flags.
        access_flags: RiscvAccessFlags,
    },

    /// Guest requested supervisor software interrupts for other harts.
    SendIpi(RiscvIpiRequest),
    /// SBI work interpreted only after unloading the hardware backend.
    SbiCall(RiscvSbiCall),
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
    CpuDown,
    /// Retentive SBI hart suspend returns an ABI success after wake.
    SbiStandby,
    /// Guest requested system shutdown.
    SystemDown,
    /// No host-visible action is needed.
    Nothing,
}

/// An exit record owned by one RISC-V vCPU task after hardware unloading.
///
/// Unlike [`RiscvVmExit`], routing-dependent fields are resolved while the
/// topology and hardware backend are still owned by the exiting task. The
/// unbound handler therefore never queries the complete VM.
#[derive(Debug)]
pub enum RiscvExit {
    /// Guest issued a hypercall.
    Hypercall { nr: u64, args: [u64; 6] },
    /// Guest MMIO read.
    MmioRead {
        addr: RiscvGuestPhysAddr,
        width: RiscvAccessWidth,
        reg: usize,
        reg_width: RiscvAccessWidth,
        signed_ext: bool,
        /// Instruction length retired only after the device access succeeds.
        advance: usize,
    },
    /// Guest MMIO write.
    MmioWrite {
        addr: RiscvGuestPhysAddr,
        width: RiscvAccessWidth,
        data: u64,
        /// Instruction length retired only after the device access succeeds.
        advance: usize,
        touches_vplic: bool,
    },
    /// Guest-stage page fault that was not decoded as an MMIO access.
    NestedPageFault {
        addr: RiscvGuestPhysAddr,
        access_flags: RiscvAccessFlags,
    },
    /// Guest requested software interrupts on a pre-resolved hart set.
    SendIpi {
        request: RiscvIpiRequest,
        /// `None` means topology validation rejected the complete request.
        targets: Option<std::boxed::Box<[usize]>>,
    },
    /// Guest requested another hart to start.
    CpuOn {
        /// `None` means the guest hart is absent from the fixed topology.
        target_vcpu_id: Option<usize>,
        entry_point: RiscvGuestPhysAddr,
        context_id: usize,
    },
    /// Guest requested this hart to stop.
    CpuOff,
    /// An SBI console request whose guest memory is accessed in task context.
    SbiCall(RiscvSbiCall),
    /// Task-side forwarding produced an owned ABI result.
    SbiResult { error: usize, value: usize },
    /// Retentive SBI hart suspend completes its ABI before parking.
    SbiStandby,
    /// Guest requested system shutdown.
    SystemDown,
    /// Re-enter the guest without a register completion.
    Nothing,
}

/// Register effects committed by the vCPU owner before the next guest entry.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum RiscvCompletion {
    /// No register state changes.
    #[default]
    None,
    /// Writes one general-purpose register.
    Gpr { register: usize, value: usize },
    /// Retires one completed emulation.
    ///
    /// An emulated load writes its destination register when present; an
    /// emulated store only steps the instruction pointer. The instruction
    /// pointer moves solely here, so a faulted or unconsumed access keeps the
    /// original retry state.
    Retire {
        register: Option<usize>,
        value: usize,
        advance: usize,
    },
    /// Writes the two SBI result registers (`a0` and `a1`).
    ///
    /// `RegisterCompletion::Return` carries only `a0`; RISC-V maps it to an
    /// SBI result whose value register is zero so HSM retains its ABI.
    SbiRet { error: usize, value: usize },
    /// Completes a previously returned SBI IPI request with its original ABI.
    Ipi {
        request: RiscvIpiRequest,
        completion: RiscvIpiCompletion,
    },
}

impl RiscvCompletion {
    /// Attaches the emulated instruction length to a register-only completion.
    ///
    /// A portable MMIO completion carries the destination register and its
    /// captured width; this turns it into a durable retirement that also steps
    /// the guest instruction pointer by `advance` bytes.
    pub(crate) fn retire(self, advance: usize) -> Self {
        match self {
            Self::None => Self::Retire {
                register: None,
                value: 0,
                advance,
            },
            Self::Gpr { register, value } => Self::Retire {
                register: Some(register),
                value,
                advance,
            },
            other => other,
        }
    }
}

impl From<RegisterCompletion> for RiscvCompletion {
    /// Maps a portable register effect onto the RISC-V ABI.
    ///
    /// [`RegisterCompletion::Return`] carries only `a0`. The HSM extension
    /// returns its error in `a0` and zero in `a1`, so the portable return maps
    /// to an SBI result whose value register is zero.
    fn from(completion: RegisterCompletion) -> Self {
        match completion {
            RegisterCompletion::None => Self::None,
            RegisterCompletion::Gpr { register, value } => Self::Gpr { register, value },
            RegisterCompletion::Return(error) => Self::SbiRet { error, value: 0 },
        }
    }
}
