use axaddrspace::{GuestPhysAddr, MappingFlags};

#[allow(unused_imports)] // used in doc
use super::AxArchVCpu;

/// The width of an access.
///
/// Note that the term "word" here refers to 16-bit data, as in the x86 architecture.
#[derive(Debug)]
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

/// The port number of an I/O operation.
type Port = u16;

/// The result of [`AxArchVCpu::run`].
/// Can we reference or directly reuse content from [kvm-ioctls](https://github.com/rust-vmm/kvm-ioctls/blob/main/src/ioctls/vcpu.rs) ?
#[non_exhaustive]
#[derive(Debug)]
pub enum AxVCpuExitReason {
    /// The instruction executed by the vcpu performs a hypercall.
    Hypercall {
        /// The hypercall number.
        nr: u64,
        /// The arguments for the hypercall.
        args: [u64; 6],
    },
    /// The instruction executed by the vcpu performs a MMIO read operation.
    MmioRead {
        /// The physical address of the MMIO read.
        addr: GuestPhysAddr,
        /// The width of the MMIO read.
        width: AccessWidth,
        /// The index of reg to be read
        reg: usize,
        /// The width of the reg to be read
        reg_width: AccessWidth,
    },
    /// The instruction executed by the vcpu performs a MMIO write operation.
    MmioWrite {
        /// The physical address of the MMIO write.
        addr: GuestPhysAddr,
        /// The width of the MMIO write.
        width: AccessWidth,
        /// The data to be written.
        data: u64,
    },
    /// The instruction executed by the vcpu performs a I/O read operation.
    ///
    /// It's unnecessary to specify the destination register because it's always `al`, `ax`, or `eax` (as port-I/O exists only in x86).
    IoRead {
        /// The port number of the I/O read.
        port: Port,
        /// The width of the I/O read.
        width: AccessWidth,
    },
    /// The instruction executed by the vcpu performs a I/O write operation.
    ///
    /// It's unnecessary to specify the source register because it's always `al`, `ax`, or `eax` (as port-I/O exists only in x86).
    IoWrite {
        /// The port number of the I/O write.
        port: Port,
        /// The width of the I/O write.
        width: AccessWidth,
        /// The data to be written.
        data: u64,
    },
    /// An external interrupt happened.
    ///
    /// Note that fields may be added in the future, use `..` to handle them.
    ExternalInterrupt {
        /// The interrupt vector.
        vector: u64,
    },
    /// A nested page fault happened. (EPT violation in x86)
    ///
    /// Note that fields may be added in the future, use `..` to handle them.
    NestedPageFault {
        /// The guest physical address of the fault.
        addr: GuestPhysAddr,
        /// The access flags of the fault.
        access_flags: MappingFlags,
    },
    /// The vcpu is halted.
    Halt,
    /// Nothing special happened, the vcpu has handled the exit itself.
    ///
    /// This exists to allow the caller to have a chance to check virtual devices/physical devices/virtual interrupts.
    Nothing,
    /// Something bad happened during VM entry, the vcpu could not be run due to unknown reasons.
    /// Further architecture-specific information is available in hardware_entry_failure_reason.
    /// Corresponds to `KVM_EXIT_FAIL_ENTRY`.
    FailEntry {
        /// Architecture related VM entry failure reasons.
        hardware_entry_failure_reason: u64,
    },
}
