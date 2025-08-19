//! Structures and functions for user space.

use core::ops::{Deref, DerefMut};

use loongArch64::register::{
    badi, badv,
    estat::{self, Exception, Trap},
};
use memory_addr::VirtAddr;

use crate::{
    trap::{ExceptionKind, PageFaultFlags, ReturnReason},
    TrapFrame,
};

/// Context to enter user space.
#[derive(Debug, Clone)]
pub struct UserContext(TrapFrame);

impl UserContext {
    /// Creates an empty context with all registers set to zero.
    pub fn empty() -> Self {
        Self(Default::default())
    }

    /// Creates a new context with the given entry point, user stack pointer,
    /// and the argument.
    pub fn new(entry: usize, ustack_top: VirtAddr, arg0: usize) -> Self {
        let mut trap_frame = TrapFrame::default();
        const PPLV_UMODE: usize = 0b11;
        const PIE: usize = 1 << 2;
        trap_frame.regs.sp = ustack_top.as_usize();
        trap_frame.era = entry;
        trap_frame.prmd = PPLV_UMODE | PIE;
        trap_frame.regs.a0 = arg0;
        Self(trap_frame)
    }

    /// Enter user space.
    ///
    /// It restores the user registers and jumps to the user entry point
    /// (saved in `sepc`).
    ///
    /// This function returns when an exception or syscall occurs.
    pub fn run(&mut self) -> ReturnReason {
        extern "C" {
            fn enter_user(tf: &mut TrapFrame);
        }

        crate::asm::disable_irqs();
        unsafe { enter_user(&mut self.0) };

        let estat = estat::read();
        let badv = badv::read().vaddr();
        let badi = badi::read().inst();

        let ret = match estat.cause() {
            Trap::Interrupt(_) => {
                let irq_num: usize = estat.is().trailing_zeros() as usize;
                handle_trap!(IRQ, irq_num);
                ReturnReason::Interrupt
            }
            Trap::Exception(Exception::Syscall) => {
                self.era += 4;
                ReturnReason::Syscall
            }
            Trap::Exception(Exception::LoadPageFault)
            | Trap::Exception(Exception::PageNonReadableFault) => {
                ReturnReason::PageFault(va!(badv), PageFaultFlags::READ | PageFaultFlags::USER)
            }
            Trap::Exception(Exception::StorePageFault)
            | Trap::Exception(Exception::PageModifyFault) => {
                ReturnReason::PageFault(va!(badv), PageFaultFlags::WRITE | PageFaultFlags::USER)
            }
            Trap::Exception(Exception::FetchPageFault)
            | Trap::Exception(Exception::PageNonExecutableFault) => {
                ReturnReason::PageFault(va!(badv), PageFaultFlags::EXECUTE | PageFaultFlags::USER)
            }
            Trap::Exception(e) => ReturnReason::Exception(ExceptionInfo { e, badv, badi }),
            _ => ReturnReason::Unknown,
        };

        crate::asm::enable_irqs();
        ret
    }
}

impl Deref for UserContext {
    type Target = TrapFrame;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for UserContext {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl From<TrapFrame> for UserContext {
    fn from(tf: TrapFrame) -> Self {
        Self(tf)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ExceptionInfo {
    pub e: Exception,
    pub badv: usize,
    pub badi: u32,
}

impl ExceptionInfo {
    pub fn kind(&self) -> ExceptionKind {
        match self.e {
            Exception::Breakpoint => ExceptionKind::Breakpoint,
            Exception::InstructionNotExist | Exception::InstructionPrivilegeIllegal => {
                ExceptionKind::IllegalInstruction
            }
            Exception::AddressNotAligned => ExceptionKind::Misaligned,
            _ => ExceptionKind::Other,
        }
    }
}
