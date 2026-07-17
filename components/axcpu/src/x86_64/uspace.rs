//! Structures and functions for user space.

use core::{
    mem::{align_of, offset_of, size_of},
    ops::{Deref, DerefMut},
};

use ax_memory_addr::VirtAddr;
use x86_64::{
    registers::{
        control::Cr2,
        model_specific::{Efer, EferFlags, LStar, SFMask, Star},
        rflags::RFlags,
    },
    structures::idt::ExceptionVector,
};

use super::{
    TrapFrame, gdt,
    trap::{IRQ_VECTOR_END, IRQ_VECTOR_START, LEGACY_SYSCALL_VECTOR, err_code_to_flags},
};
pub use crate::uspace_common::{
    DecodedUserExit, ExceptionKind, ExceptionSyndrome, RawUserExit, RawUserInterrupt,
    UserExitReason,
};

/// Context to enter user space.
#[derive(Debug, Clone, Copy)]
#[repr(C, align(16))]
pub struct UserContext {
    tf: TrapFrame,
    /// FS Segment Base
    pub fs_base: u64,
    /// GS Segment Base
    pub gs_base: u64,
    /// Kernel FS base saved and restored exclusively by `enter_user`.
    kernel_fs_base: u64,
}

const _: () = {
    // A privilege transition may align TSS.RSP0 down to 16 bytes before
    // constructing the hardware frame. `enter_user` uses the end of `tf` as
    // both RSP0 and the boundary above which it saves the kernel continuation,
    // so both the object and that boundary must already be aligned.
    assert!(align_of::<UserContext>() >= 16);
    assert!(size_of::<TrapFrame>().is_multiple_of(16));
    assert!(offset_of!(UserContext, tf) == 0);
    assert!(offset_of!(UserContext, fs_base) == size_of::<TrapFrame>());
    assert!(offset_of!(UserContext, gs_base) == size_of::<TrapFrame>() + size_of::<u64>());
    assert!(
        offset_of!(UserContext, kernel_fs_base) == size_of::<TrapFrame>() + 2 * size_of::<u64>()
    );
};

impl UserContext {
    /// Creates a new context with the given entry point, user stack pointer,
    /// and the argument.
    pub fn new(entry: usize, ustack_top: VirtAddr, arg0: usize) -> Self {
        use x86_64::registers::rflags::RFlags;
        Self {
            tf: TrapFrame {
                rdi: arg0 as _,
                rip: entry as _,
                cs: gdt::UCODE64.0 as _,
                rflags: RFlags::INTERRUPT_FLAG.bits(), // IOPL = 0, IF = 1
                rsp: ustack_top.as_usize() as _,
                ss: gdt::UDATA.0 as _,
                ..Default::default()
            },
            fs_base: 0,
            gs_base: 0,
            kernel_fs_base: 0,
        }
    }

    /// Normalizes a cloned user context so it can safely return to ring 3.
    pub fn prepare_clone_child_return_state(&mut self) {
        let mut flags = RFlags::from_bits_truncate(self.tf.rflags);
        flags.insert(RFlags::INTERRUPT_FLAG);
        flags.remove(RFlags::TRAP_FLAG | RFlags::NESTED_TASK | RFlags::RESUME_FLAG);
        self.tf.rflags = flags.bits();
    }

    /// Clears the single-step trap flag after a debug exception.
    ///
    /// Returns whether the flag had been set in the saved user context.
    pub fn clear_single_step_after_debug(&mut self) -> bool {
        let mut flags = RFlags::from_bits_truncate(self.tf.rflags);
        let was_set = flags.contains(RFlags::TRAP_FLAG);
        flags.remove(RFlags::TRAP_FLAG);
        self.tf.rflags = flags.bits();
        was_set
    }

    /// Returns the syscall instruction length in bytes.
    pub const fn syscall_insn_len(&self) -> usize {
        2
    }

    /// Gets the TLS area.
    pub const fn tls(&self) -> usize {
        self.fs_base as _
    }

    /// Sets the TLS area.
    pub const fn set_tls(&mut self, tls_area: usize) {
        self.fs_base = tls_area as _;
    }

    /// Enters user space.
    ///
    /// It restores the user registers and jumps to the user entry point
    /// (saved in `rip`).
    ///
    /// This function returns an opaque context-bound token with raw local
    /// interrupts still masked. It does not read or dispatch the captured
    /// exception. The runtime must publish kernel accounting before calling
    /// [`Self::decode_raw_exit`].
    pub fn run_raw(&mut self) -> RawUserExit {
        unsafe extern "C" {
            fn enter_user(uctx: &mut UserContext);
        }

        assert_eq!(self.cs, gdt::UCODE64.0 as _);
        assert_eq!(self.ss, gdt::UDATA.0 as _);

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

        let vector = self.vector as u8;

        const PAGE_FAULT_VECTOR: u8 = ExceptionVector::Page as u8;

        match (vector, err_code_to_flags(self.error_code)) {
            (PAGE_FAULT_VECTOR, Ok(flags)) => DecodedUserExit::Reason(UserExitReason::PageFault(
                va!(Cr2::read_raw() as usize),
                flags,
            )),
            (LEGACY_SYSCALL_VECTOR, _) => DecodedUserExit::Reason(UserExitReason::Syscall),
            (IRQ_VECTOR_START..=IRQ_VECTOR_END, _) => {
                DecodedUserExit::Interrupt(RawUserInterrupt::new(vector as _))
            }
            _ => DecodedUserExit::Reason(UserExitReason::Exception(ExceptionInfo {
                vector,
                error_code: self.error_code,
                cr2: Cr2::read_raw() as usize,
            })),
        }
    }
}

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
    /// The exception vector.
    pub vector: u8,
    /// The error code.
    pub error_code: u64,
    /// The faulting virtual address (if applicable).
    pub cr2: usize,
}

impl ExceptionInfo {
    /// Returns the faulting virtual address when the CPU records one.
    pub const fn fault_addr(&self) -> Option<usize> {
        Some(self.cr2)
    }

    /// Returns architecture-neutral syndrome information for this exception.
    pub const fn syndrome(&self) -> ExceptionSyndrome {
        ExceptionSyndrome {
            raw: self.error_code,
            class: self.vector as u64,
            iss: 0,
        }
    }

    /// Returns a generalized kind of this exception.
    pub fn kind(&self) -> ExceptionKind {
        match ExceptionVector::try_from(self.vector) {
            Ok(ExceptionVector::Debug) => ExceptionKind::Debug,
            Ok(ExceptionVector::Breakpoint) => ExceptionKind::Breakpoint,
            Ok(ExceptionVector::InvalidOpcode) => ExceptionKind::IllegalInstruction,
            // `#DE`: integer divide-by-zero / `INT_MIN / -1`. Linux delivers this
            // as SIGFPE/FPE_INTDIV; the HotSpot JVM's x86 interpreter and JIT
            // rely on the trap to raise Java `ArithmeticException`.
            Ok(ExceptionVector::Division) => ExceptionKind::ArithmeticError,
            _ => ExceptionKind::Other,
        }
    }
}

/// Initializes syscall support and setups the syscall handler.
pub(super) fn init_syscall() {
    unsafe extern "C" {
        fn syscall_entry();
    }

    LStar::write(x86_64::VirtAddr::new_truncate(
        syscall_entry as *const () as usize as _,
    ));
    Star::write(gdt::UCODE64, gdt::UDATA, gdt::KCODE64, gdt::KDATA).unwrap();
    SFMask::write(
        RFlags::TRAP_FLAG
            | RFlags::INTERRUPT_FLAG
            | RFlags::DIRECTION_FLAG
            | RFlags::IOPL_LOW
            | RFlags::IOPL_HIGH
            | RFlags::NESTED_TASK
            | RFlags::ALIGNMENT_CHECK,
    ); // TF | IF | DF | IOPL | AC | NT (0x47700)
    unsafe {
        Efer::update(|efer| *efer |= EferFlags::SYSTEM_CALL_EXTENSIONS);
    }
}
