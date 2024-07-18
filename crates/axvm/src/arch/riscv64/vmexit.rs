use riscv::register::mcause::Interrupt;

use crate::{GuestPhysAddr, GuestVirtAddr};
use tock_registers::LocalRegisterCopy;

use super::{csrs::defs::hstatus, sbi::SbiMessage};

/// The privilege level at the time a trap occurred, as reported in sstatus.SPP or hstatus.SPVP.
#[derive(Copy, Clone, Debug)]
#[repr(u64)]
pub enum PrivilegeLevel {
    User = 0,
    Supervisor = 1,
}

impl PrivilegeLevel {
    pub fn from_hstatus(csr: usize) -> Self {
        let val = LocalRegisterCopy::<usize, hstatus::Register>::new(csr);
        match val.read(hstatus::spvp) {
            0 => Self::User,
            1 => Self::Supervisor,
            _ => unreachable!(), // Field is only 1-bit wide.
        }
    }
}

#[derive(Debug, Clone, Copy)]
/// Identifies the reason for a trap taken from a vCPU.
pub enum VmExitInfo {
    /// ECALLs from VS mode.
    Ecall(Option<SbiMessage>),
    /// G-stage page faluts
    PageFault {
        /// Page fault addr.
        fault_addr: GuestPhysAddr,
        /// Page fault inst addr.
        falut_pc: GuestVirtAddr,
        /// Page fault inst.
        inst: u32,
        /// Page fault privilege level.
        priv_level: PrivilegeLevel,
    },
    /// Instruction emulation trap
    VirtualInstruction {
        /// Virtual instruction addr.
        fault_pc: GuestVirtAddr,
        /// Virtual instruction privilege level.
        priv_level: PrivilegeLevel,
    },
    /// An interrupt intended for the vCPU's host.
    HostInterruot(Interrupt),
    /// An timer interrupt for the running vCPU that can't be delegated and must be injected. The
    /// interrupt is injected the vCPU is run.
    TimerInterruptEmulation,
    /// An external interrupt for the running vCPU that can't be delegated and must be injected.
    ExternalInterruptEmulation,
}
