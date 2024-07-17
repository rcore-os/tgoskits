#[allow(unused_imports)] // used in doc
use super::AxArchVCpu;
use super::GuestPhysAddr;

/// The width of an access.
///
/// Note that the term "word" here refers to 16-bit data, as in the x86 architecture.
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
#[non_exhaustive]
pub enum AxArchVCpuExitReason {
    /// The instruction executed by the vcpu performs a MMIO read operation.
    MmioRead {
        /// The physical address of the MMIO read.
        addr: GuestPhysAddr,
        /// The width of the MMIO read.
        width: AccessWidth,
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
        vector: u8,
    },
    /// A nested page fault happened. (EPT violation in x86)
    ///
    /// Note that fields may be added in the future, use `..` to handle them.
    NestedPageFault {
        /// The guest physical address of the fault.
        addr: GuestPhysAddr,
    },
    /// The vcpu is halted.
    Halt,
    /// Nothing special happened, the vcpu has handled the exit itself.
    ///
    /// This exists to allow the caller to have a chance to check virtual devices/physical devices/virtual interrupts.
    Nothing,
}
