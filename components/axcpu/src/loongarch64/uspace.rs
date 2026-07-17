//! Structures and functions for user space.

use core::ops::{Deref, DerefMut};

use ax_memory_addr::VirtAddr;
use loongArch64::register::{
    badi, badv,
    estat::{self, Exception, Trap},
};

pub use crate::uspace_common::{
    DecodedUserExit, ExceptionKind, ExceptionSyndrome, RawUserExit, RawUserInterrupt,
    UserExitReason,
};
use crate::{TrapFrame, trap::PageFaultFlags};

const ECODE_LSX_DISABLED: usize = 0x10;
const ECODE_LASX_DISABLED: usize = 0x11;
const ECODE_BINARY_TRANSLATION_DISABLED: usize = 0x14;

/// Context to enter user space.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct UserContext(TrapFrame);

impl UserContext {
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

    /// Normalizes a cloned user context so it can safely return to user mode.
    pub fn prepare_clone_child_return_state(&mut self) {
        const PPLV_MASK: usize = 0b11;
        const PIE: usize = 1 << 2;
        self.0.prmd = (self.0.prmd & !PPLV_MASK) | PPLV_MASK | PIE;
    }

    /// Clears any architecture single-step state after a debug exception.
    ///
    /// LoongArch single-step is currently emulated by temporarily patching a
    /// `break`, so there is no saved CPU flag to clear here.
    pub const fn clear_single_step_after_debug(&mut self) -> bool {
        false
    }

    /// Returns the syscall instruction length in bytes.
    pub const fn syscall_insn_len(&self) -> usize {
        4
    }

    /// Enter user space.
    ///
    /// It restores the user registers and jumps to the user entry point
    /// (saved in `sepc`).
    ///
    /// This function returns an opaque context-bound token with raw local
    /// interrupts still masked. It does not read or dispatch the captured
    /// exception. The runtime must publish kernel accounting before calling
    /// [`Self::decode_raw_exit`].
    pub fn run_raw(&mut self) -> RawUserExit {
        unsafe extern "C" {
            fn enter_user(uctx: &mut UserContext);
        }

        crate::asm::disable_irqs();
        unsafe { enter_user(self) };

        RawUserExit::bind(self, 0)
    }

    /// Decodes a raw exit previously returned by this context.
    ///
    /// The caller must keep raw local interrupts masked and publish the
    /// user-to-kernel accounting transition before invoking this method.
    ///
    /// # Panics
    ///
    /// Panics if `raw_exit` was produced by a different [`UserContext`].
    pub fn decode_raw_exit(&mut self, raw_exit: RawUserExit) -> DecodedUserExit {
        raw_exit.assert_bound_to(self);

        let estat = estat::read();
        let badv = badv::read().vaddr();
        let badi = badi::read().inst();
        let ecode = estat.ecode();
        let esubcode = estat.esubcode();

        match estat.cause() {
            Trap::Interrupt(_) => {
                let irq_num: usize = estat.is().trailing_zeros() as usize;
                DecodedUserExit::Interrupt(RawUserInterrupt::new(irq_num))
            }
            Trap::Exception(Exception::Syscall) => {
                self.era += 4;
                DecodedUserExit::Reason(UserExitReason::Syscall)
            }
            Trap::Exception(Exception::LoadPageFault)
            | Trap::Exception(Exception::PageNonReadableFault) => DecodedUserExit::Reason(
                UserExitReason::PageFault(va!(badv), PageFaultFlags::READ | PageFaultFlags::USER),
            ),
            Trap::Exception(Exception::StorePageFault)
            | Trap::Exception(Exception::PageModifyFault) => DecodedUserExit::Reason(
                UserExitReason::PageFault(va!(badv), PageFaultFlags::WRITE | PageFaultFlags::USER),
            ),
            Trap::Exception(Exception::FetchPageFault)
            | Trap::Exception(Exception::PageNonExecutableFault) => {
                DecodedUserExit::Reason(UserExitReason::PageFault(
                    va!(badv),
                    PageFaultFlags::EXECUTE | PageFaultFlags::USER,
                ))
            }
            Trap::Exception(Exception::PagePrivilegeIllegal) => {
                // The CPU reports only a privilege mismatch here, not whether
                // the original access was a load, store, or fetch. An unmapped
                // user access can also arrive here after the low-level TLB
                // refill path installs a non-user placeholder entry. Treat it
                // as a user page fault so the VM layer can populate a lazy user
                // mapping or reject a real permission violation. Flush the
                // address first in case the exception came from such an entry
                // or a stale kernel-only TLB entry for the same VA.
                crate::asm::flush_tlb(Some(va!(badv)));
                DecodedUserExit::Reason(UserExitReason::PageFault(va!(badv), PageFaultFlags::USER))
            }
            Trap::Exception(e) => {
                DecodedUserExit::Reason(UserExitReason::Exception(ExceptionInfo {
                    e,
                    badv,
                    badi,
                    ecode,
                    esubcode,
                }))
            }
            Trap::Unknown
                if matches!(
                    ecode,
                    ECODE_LSX_DISABLED | ECODE_LASX_DISABLED | ECODE_BINARY_TRANSLATION_DISABLED
                ) =>
            {
                DecodedUserExit::Reason(UserExitReason::Exception(ExceptionInfo {
                    e: Exception::InstructionNotExist,
                    badv,
                    badi,
                    ecode,
                    esubcode,
                }))
            }
            _ => DecodedUserExit::Reason(UserExitReason::Unknown),
        }
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

/// Information about an exception that occurred in user space.
#[derive(Debug, Clone, Copy)]
pub struct ExceptionInfo {
    /// The raw exception.
    pub e: Exception,
    /// The faulting address (from `badv`).
    pub badv: usize,
    /// The instruction causing the fault (from `badi`).
    pub badi: u32,
    /// The raw exception code from `estat`.
    pub ecode: usize,
    /// The raw exception subcode from `estat`.
    pub esubcode: usize,
}

impl ExceptionInfo {
    /// Returns the faulting virtual address when the CPU records one.
    pub const fn fault_addr(&self) -> Option<usize> {
        Some(self.badv)
    }

    /// Returns architecture-neutral syndrome information for this exception.
    pub const fn syndrome(&self) -> ExceptionSyndrome {
        ExceptionSyndrome {
            raw: self.ecode as u64,
            class: self.ecode as u64,
            iss: self.esubcode as u64,
        }
    }

    /// Returns a generalized kind of this exception.
    pub fn kind(&self) -> ExceptionKind {
        if matches!(
            self.ecode,
            ECODE_LSX_DISABLED | ECODE_LASX_DISABLED | ECODE_BINARY_TRANSLATION_DISABLED
        ) {
            return ExceptionKind::IllegalInstruction;
        }
        match self.e {
            Exception::Breakpoint => ExceptionKind::Breakpoint,
            Exception::InstructionNotExist
            | Exception::InstructionPrivilegeIllegal
            | Exception::FloatingPointUnavailable => ExceptionKind::IllegalInstruction,
            Exception::AddressNotAligned => ExceptionKind::Misaligned,
            _ => ExceptionKind::Other,
        }
    }
}
