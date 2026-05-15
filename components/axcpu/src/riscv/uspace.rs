//! Structures and functions for user space.

use core::ops::{Deref, DerefMut};

use ax_memory_addr::VirtAddr;
#[cfg(feature = "fp-simd")]
use riscv::register::sstatus::FS;
use riscv::{
    interrupt::{
        Trap,
        supervisor::{Exception as E, Interrupt as I},
    },
    register::{scause, sstatus::Sstatus, stval},
};

pub use crate::uspace_common::{ExceptionKind, ReturnReason};
use crate::{GeneralRegisters, TrapFrame, trap::PageFaultFlags};

/// Context to enter user space.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct UserContext(TrapFrame);

impl UserContext {
    /// Creates a new context with the given entry point, user stack pointer,
    /// and the argument.
    pub fn new(entry: usize, ustack_top: VirtAddr, arg0: usize) -> Self {
        let mut sstatus = Sstatus::from_bits(0);
        sstatus.set_spie(true); // enable interrupts
        sstatus.set_sum(true); // enable user memory access in supervisor mode
        #[cfg(feature = "fp-simd")]
        sstatus.set_fs(FS::Initial); // set the FPU to initial state

        #[cfg(feature = "xuantie-c9xx")]
        // enable vector status bits of sstatus
        Self::set_sstatus(&mut sstatus, 0x3 << 23, false);

        Self(TrapFrame {
            regs: GeneralRegisters {
                a0: arg0,
                sp: ustack_top.as_usize(),
                ..Default::default()
            },
            sepc: entry,
            sstatus,
        })
    }

    /// Normalizes a cloned user context so it can safely return to user mode.
    pub fn prepare_clone_child_return_state(&mut self) {
        self.0.sstatus.set_spie(true);
        self.0.sstatus.set_sum(true);
        #[cfg(feature = "fp-simd")]
        if matches!(self.0.sstatus.fs(), FS::Off) {
            self.0.sstatus.set_fs(FS::Initial);
        }
    }

    /// Enter user space.
    ///
    /// It restores the user registers and jumps to the user entry point
    /// (saved in `sepc`).
    ///
    /// This function returns when an exception or syscall occurs.
    pub fn run(&mut self) -> ReturnReason {
        unsafe extern "C" {
            fn enter_user(uctx: &mut UserContext);
        }

        // Refresh all instruction caches before entering the user program space to resolve user program errors
        riscv::asm::fence_i();

        crate::asm::disable_irqs();
        unsafe { enter_user(self) };

        let scause = scause::read();
        let ret = if let Ok(cause) = scause.cause().try_into::<I, E>() {
            let stval = stval::read();
            match cause {
                Trap::Interrupt(_) => {
                    crate::trap::irq_handler(scause.bits());
                    ReturnReason::Interrupt
                }
                Trap::Exception(E::UserEnvCall) => {
                    self.sepc += 4;
                    ReturnReason::Syscall
                }
                Trap::Exception(E::LoadPageFault) => {
                    ReturnReason::PageFault(va!(stval), PageFaultFlags::READ | PageFaultFlags::USER)
                }
                Trap::Exception(E::StorePageFault) => ReturnReason::PageFault(
                    va!(stval),
                    PageFaultFlags::WRITE | PageFaultFlags::USER,
                ),
                Trap::Exception(E::InstructionPageFault) => ReturnReason::PageFault(
                    va!(stval),
                    PageFaultFlags::EXECUTE | PageFaultFlags::USER,
                ),
                Trap::Exception(e) => ReturnReason::Exception(ExceptionInfo { e, stval }),
            }
        } else {
            ReturnReason::Unknown
        };

        crate::asm::enable_irqs();
        ret
    }

    /// Sets the sstatus register.
    /// Due to the restriction of Sstatus struct, some bits of the sstatus register cannot be effectively set,
    /// So this function can effectively set the required bits of sstatus.
    pub fn set_sstatus(sstatus: &mut Sstatus, bits: usize, is_clear: bool) {
        if bits == 0 {
            log::error!("Invalid parameter: {:x}", bits);
            return;
        }
        unsafe {
            let sstatus_ptr = sstatus as *mut Sstatus as *mut usize;
            if is_clear {
                *sstatus_ptr &= !bits;
            } else {
                *sstatus_ptr |= bits;
            }
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
    pub e: E,
    /// The faulting address (from `stval`).
    pub stval: usize,
}

impl ExceptionInfo {
    /// Returns a generalized kind of this exception.
    pub fn kind(&self) -> ExceptionKind {
        match self.e {
            E::Breakpoint => ExceptionKind::Breakpoint,
            E::IllegalInstruction => ExceptionKind::IllegalInstruction,
            E::InstructionMisaligned | E::LoadMisaligned | E::StoreMisaligned => {
                ExceptionKind::Misaligned
            }
            _ => ExceptionKind::Other,
        }
    }
}
