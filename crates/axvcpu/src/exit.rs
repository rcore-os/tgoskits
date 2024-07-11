use super::GuestPhysAddr;

/// The width of an access.
///
/// Note that the term "word" here refers to 16-bit data, as in the x86 architecture.
pub enum AccessWidth {
    Byte,
    Word,
    Dword,
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

type Port = u16;

/// The result of `AxArchVCpu::run`.
#[non_exhaustive]
pub enum AxArchVCpuExitReason {
    /// The instruction executed by the vcpu performs a MMIO read operation.
    MmioRead {
        addr: GuestPhysAddr,
        width: AccessWidth,
    },
    /// The instruction executed by the vcpu performs a MMIO write operation.
    MmioWrite {
        addr: GuestPhysAddr,
        width: AccessWidth,
        data: u64,
    },
    /// The instruction executed by the vcpu performs a I/O read operation.
    ///
    /// It's unnecessary to specify the destination register because it's always `al`, `ax`, or `eax`.
    IoRead { port: Port, width: AccessWidth },
    /// The instruction executed by the vcpu performs a I/O write operation.
    ///
    /// It's unnecessary to specify the source register because it's always `al`, `ax`, or `eax`.
    IoWrite {
        port: Port,
        width: AccessWidth,
        data: u64,
    },
    /// The vcpu is halted.
    Halt,
    /// Nothing special happened, the vcpu itself has handled the exit itself.
    /// 
    /// This exists to allow the caller to have a chance to check virtual devices/physical devices/virtual interrupts.
    Nothing,
}
