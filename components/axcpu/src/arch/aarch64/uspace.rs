//! Structures and functions for user space.

use core::{
    mem::{offset_of, size_of},
    ops::{Deref, DerefMut},
};

use aarch64_cpu::registers::FAR_EL1;
pub use aarch64_cpu::registers::{ESR_EL1, Readable};
use ax_memory_addr::VirtAddr;
pub use tock_registers::{
    LocalRegisterCopy, RegisterLongName, UIntLike,
    debug::{RegisterDebugInfo, RegisterDebugValue},
    fields::{Field, FieldValue, TryFromValue},
};

use super::trap::{TrapKind, is_valid_page_fault};
pub use crate::uspace_common::{ExceptionKind, ExceptionSyndrome, ReturnReason};
use crate::{arch::current::context::TrapFrame, trap::PageFaultFlags};

/// Context to enter user space.
#[repr(C, align(16))]
#[derive(Debug, Clone, Copy)]
pub struct UserContext {
    tf: TrapFrame,
    /// Stack Pointer (SP_EL0).
    pub sp: u64,
    /// Software Thread ID Register (TPIDR_EL0).
    pub tpidr: u64,
}

// SAFETY: `TrapFrame`, `sp`, and `tpidr` are contiguous integer storage and
// their combined size is already a multiple of the declared 16-byte alignment.
unsafe impl bytemuck::NoUninit for UserContext {}

const _: () = {
    assert!(size_of::<TrapFrame>() == 34 * size_of::<u64>());
    assert!(offset_of!(UserContext, tf) == 0);
    assert!(offset_of!(UserContext, sp) == size_of::<TrapFrame>());
    assert!(offset_of!(UserContext, tpidr) == size_of::<TrapFrame>() + size_of::<u64>());
    assert!(size_of::<UserContext>() == size_of::<TrapFrame>() + 2 * size_of::<u64>());
};

impl UserContext {
    /// Creates a new user context with the given entry point, stack top, and argument.
    pub fn new(entry: usize, ustack_top: VirtAddr, arg0: usize) -> Self {
        use aarch64_cpu::registers::SPSR_EL1;
        let mut regs = [0; 31];
        regs[0] = arg0 as _;
        Self {
            tf: TrapFrame {
                x: regs,
                elr: entry as _,
                spsr: (SPSR_EL1::M::EL0t
                    + SPSR_EL1::D::Masked
                    + SPSR_EL1::A::Masked
                    + SPSR_EL1::I::Unmasked
                    + SPSR_EL1::F::Masked)
                    .value,
                sp: 0,
            },
            sp: ustack_top.as_usize() as _,
            tpidr: 0,
        }
    }

    /// Normalizes a cloned user context so it can safely return to EL0.
    pub fn prepare_clone_child_return_state(&mut self) {
        use aarch64_cpu::registers::SPSR_EL1;

        self.tf.spsr = (self.tf.spsr
            & !(SPSR_EL1::M.mask
                | SPSR_EL1::D.mask
                | SPSR_EL1::A.mask
                | SPSR_EL1::I.mask
                | SPSR_EL1::F.mask))
            | (SPSR_EL1::M::EL0t
                + SPSR_EL1::D::Masked
                + SPSR_EL1::A::Masked
                + SPSR_EL1::I::Unmasked
                + SPSR_EL1::F::Masked)
                .value;
    }

    /// Clears any architecture single-step state after a debug exception.
    ///
    /// AArch64 user single-step is currently emulated by the Starry ptrace layer,
    /// so there is no saved CPU flag to clear here.
    pub const fn clear_single_step_after_debug(&mut self) -> bool {
        false
    }

    /// Returns the syscall instruction length in bytes.
    pub const fn syscall_insn_len(&self) -> usize {
        4
    }

    /// Gets the stack pointer.
    pub const fn sp(&self) -> usize {
        self.sp as _
    }

    /// Sets the stack pointer.
    pub const fn set_sp(&mut self, sp: usize) {
        self.sp = sp as _;
    }

    /// Gets the TLS area.
    pub const fn tls(&self) -> usize {
        self.tpidr as _
    }

    /// Sets the TLS area.
    pub const fn set_tls(&mut self, tls: usize) {
        self.tpidr = tls as _;
    }

    /// Returns whether this register image can be restored as an interruptible
    /// EL0 context.
    pub fn has_interruptible_user_return_mode(&self) -> bool {
        use aarch64_cpu::registers::SPSR_EL1;

        let runtime_daif =
            SPSR_EL1::D::Masked + SPSR_EL1::A::Masked + SPSR_EL1::I::Unmasked + SPSR_EL1::F::Masked;
        self.tf.spsr & SPSR_EL1::M::EL0t.mask() == SPSR_EL1::M::EL0t.value
            && self.tf.spsr & runtime_daif.mask() == runtime_daif.value
    }

    /// Enters user space without validating the runtime transition.
    ///
    /// It restores the user registers and jumps to the user entry point
    /// (saved in `elr`).
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
    /// task and keep every user address referenced by `self` valid. SPSR must
    /// describe an interruptible EL0 return. No code may run between those
    /// validations and this call.
    pub unsafe fn run_unchecked(&mut self) -> ReturnReason {
        unsafe extern "C" {
            fn enter_user(uctx: &mut UserContext) -> TrapKind;
        }

        assert!(
            !crate::asm::irqs_enabled(),
            "raw user entry requires the prepared IRQ-off boundary"
        );
        assert!(
            self.has_interruptible_user_return_mode(),
            "raw user entry requires an interruptible EL0 register image"
        );
        let kind = unsafe { enter_user(self) };

        let ret = match kind {
            TrapKind::Irq => {
                crate::trap::dispatch_irq(
                    0,
                    crate::trap::TrapOrigin::User,
                    Some(crate::trap::InterruptedContext {
                        pc: self.tf.ip(),
                        sp: self.sp as usize,
                        fp: self.tf.x[29] as usize,
                        privilege: crate::trap::InterruptedPrivilege::User,
                    }),
                );
                ReturnReason::Interrupt
            }
            TrapKind::Fiq | TrapKind::SError => ReturnReason::Unknown,
            TrapKind::Synchronous => {
                let esr = ESR_EL1.extract();
                let far = FAR_EL1.get() as usize;

                let iss = esr.read(ESR_EL1::ISS);

                match esr.read_as_enum(ESR_EL1::EC) {
                    Some(ESR_EL1::EC::Value::SVC64) => ReturnReason::Syscall,
                    Some(ESR_EL1::EC::Value::InstrAbortLowerEL) if is_valid_page_fault(iss) => {
                        ReturnReason::PageFault(
                            va!(far),
                            PageFaultFlags::EXECUTE | PageFaultFlags::USER,
                        )
                    }
                    Some(ESR_EL1::EC::Value::DataAbortLowerEL) if is_valid_page_fault(iss) => {
                        let wnr = (iss & (1 << 6)) != 0; // WnR: Write not Read
                        let cm = (iss & (1 << 8)) != 0; // CM: Cache maintenance
                        ReturnReason::PageFault(
                            va!(far),
                            if wnr & !cm {
                                PageFaultFlags::WRITE
                            } else {
                                PageFaultFlags::READ
                            } | PageFaultFlags::USER,
                        )
                    }
                    _ => ReturnReason::Exception(ExceptionInfo { esr, far }),
                }
            }
        };

        crate::asm::enable_irqs();
        ret
    }
}

const _: unsafe fn(&mut UserContext) -> ReturnReason = UserContext::run_unchecked;

impl Deref for UserContext {
    type Target = TrapFrame;

    fn deref(&self) -> &Self::Target {
        &self.tf
    }
}

impl DerefMut for UserContext {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.tf
    }
}

/// Information about an exception that occurred in user space.
#[derive(Debug, Clone, Copy)]
pub struct ExceptionInfo {
    /// Exception Syndrome Register
    pub esr: LocalRegisterCopy<u64, ESR_EL1::Register>,
    /// Fault Address Register
    pub far: usize,
}

impl ExceptionInfo {
    /// Returns the faulting virtual address when the CPU records one.
    pub const fn fault_addr(&self) -> Option<usize> {
        Some(self.far)
    }

    /// Returns architecture-neutral syndrome information for this exception.
    pub fn syndrome(&self) -> ExceptionSyndrome {
        ExceptionSyndrome {
            raw: self.esr_value(),
            class: self.ec_value(),
            iss: self.iss_value(),
        }
    }

    /// Returns the raw Exception Syndrome Register value.
    pub fn esr_value(&self) -> u64 {
        self.esr.get()
    }

    /// Returns the raw exception class bits.
    pub fn ec_value(&self) -> u64 {
        self.esr.read(ESR_EL1::EC)
    }

    /// Returns the instruction specific syndrome bits.
    pub fn iss_value(&self) -> u64 {
        self.esr.read(ESR_EL1::ISS)
    }

    /// Returns a generalized kind of this exception.
    pub fn kind(&self) -> ExceptionKind {
        match self.esr.read_as_enum(ESR_EL1::EC) {
            Some(ESR_EL1::EC::Value::Brk64) | Some(ESR_EL1::EC::Value::Bkpt32) => {
                ExceptionKind::Breakpoint
            }
            Some(ESR_EL1::EC::Value::IllegalExecutionState) | Some(ESR_EL1::EC::Value::Unknown) => {
                ExceptionKind::IllegalInstruction
            }
            Some(ESR_EL1::EC::Value::PCAlignmentFault)
            | Some(ESR_EL1::EC::Value::SPAlignmentFault) => ExceptionKind::Misaligned,
            _ => ExceptionKind::Other,
        }
    }
}
