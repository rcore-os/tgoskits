//! Structures and functions for user space.

use core::{
    mem::size_of,
    ops::{Deref, DerefMut},
};

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

pub use crate::uspace_common::{ExceptionKind, ExceptionSyndrome, ReturnReason};
use crate::{GeneralRegisters, TrapFrame, trap::PageFaultFlags};

/// Context to enter user space.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct UserContext(TrapFrame);

// SAFETY: `TrapFrame` is a contiguous C-layout register image containing only
// integer-backed register values and has no padding.
unsafe impl bytemuck::NoUninit for UserContext {}

const _: () = {
    assert!(size_of::<TrapFrame>() == 34 * size_of::<usize>());
    assert!(size_of::<UserContext>() == size_of::<TrapFrame>());
};

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
        {
            // Enable standard RISC-V VS plus the legacy XThead status bits used
            // by older C9xx cores. K230 C908V reports standard V in QEMU.
            const SSTATUS_VS_INITIAL: usize = 0x1 << 9;
            const XTHEAD_LEGACY_VS_MASK: usize = 0x3 << 23;
            Self::set_sstatus(
                &mut sstatus,
                SSTATUS_VS_INITIAL | XTHEAD_LEGACY_VS_MASK,
                false,
            );
        }

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

    /// Clears any architecture single-step state after a debug exception.
    ///
    /// RISC-V single-step is currently emulated by temporarily patching an
    /// `ebreak`, so there is no saved CPU flag to clear here.
    pub const fn clear_single_step_after_debug(&mut self) -> bool {
        false
    }

    /// Returns the syscall instruction length in bytes.
    pub const fn syscall_insn_len(&self) -> usize {
        4
    }

    /// Returns whether this register image can be restored as an interruptible
    /// user-mode context.
    pub fn has_interruptible_user_return_mode(&self) -> bool {
        matches!(self.0.sstatus.spp(), riscv::register::sstatus::SPP::User) && self.0.sstatus.spie()
    }

    /// Enters user space without validating the runtime transition.
    ///
    /// It restores the user registers and jumps to the user entry point
    /// (saved in `sepc`).
    ///
    /// This function returns when an exception or syscall occurs.
    ///
    /// # Safety
    ///
    /// The caller must be the runtime's prepared user-entry boundary for the
    /// current scheduler task. Its context-switch tail must be complete, no
    /// IRQ/preemption guard or hard interrupt may be active, and local IRQs
    /// must remain disabled after the final scheduler-work check. The active
    /// logical address space, hardware root and CPU footprint must match this
    /// task and keep every user address referenced by `self` valid. SSTATUS
    /// must describe an interruptible user-mode return. No code may run between
    /// those validations and this call.
    ///
    /// Safe code cannot invoke this raw boundary:
    ///
    /// ```compile_fail
    /// fn bypass_runtime(context: &mut ax_cpu::uspace::UserContext) {
    ///     context.run_unchecked();
    /// }
    /// ```
    pub unsafe fn run_unchecked(&mut self) -> ReturnReason {
        unsafe extern "C" {
            fn enter_user(uctx: &mut UserContext);
        }

        // Refresh all instruction caches before entering the user program space to resolve user program errors
        riscv::asm::fence_i();

        assert!(
            !crate::asm::irqs_enabled(),
            "raw user entry requires the prepared IRQ-off boundary"
        );
        assert!(
            self.has_interruptible_user_return_mode(),
            "raw user entry requires an interruptible user-mode register image"
        );
        unsafe { enter_user(self) };

        let scause = scause::read();
        let ret = if let Ok(cause) = scause.cause().try_into::<I, E>() {
            let stval = stval::read();
            match cause {
                Trap::Interrupt(_) => {
                    crate::trap::dispatch_irq(scause.bits());
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

const _: unsafe fn(&mut UserContext) -> ReturnReason = UserContext::run_unchecked;

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
    /// Returns the faulting virtual address when the CPU records one.
    pub const fn fault_addr(&self) -> Option<usize> {
        Some(self.stval)
    }

    /// Returns architecture-neutral syndrome information for this exception.
    pub const fn syndrome(&self) -> ExceptionSyndrome {
        ExceptionSyndrome {
            raw: 0,
            class: self.e as u64,
            iss: 0,
        }
    }

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
