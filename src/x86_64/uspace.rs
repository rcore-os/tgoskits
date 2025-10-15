//! Structures and functions for user space.

use core::ops::{Deref, DerefMut};

use memory_addr::VirtAddr;
use x86_64::{
    registers::{
        control::Cr2,
        model_specific::{Efer, EferFlags, KernelGsBase, LStar, SFMask, Star},
        rflags::RFlags,
    },
    structures::idt::ExceptionVector,
};

use super::{
    asm::{read_thread_pointer, write_thread_pointer},
    gdt,
    trap::{err_code_to_flags, IRQ_VECTOR_END, IRQ_VECTOR_START, LEGACY_SYSCALL_VECTOR},
    TrapFrame,
};

pub use crate::uspace_common::{ExceptionKind, ReturnReason};

/// Context to enter user space.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct UserContext {
    tf: TrapFrame,
    /// FS Segment Base
    pub fs_base: u64,
    /// GS Segment Base
    pub gs_base: u64,
}

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
        }
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
    /// This function returns when an exception or syscall occurs.
    pub fn run(&mut self) -> ReturnReason {
        extern "C" {
            fn enter_user(uctx: &mut UserContext);
        }

        assert_eq!(self.cs, gdt::UCODE64.0 as _);
        assert_eq!(self.ss, gdt::UDATA.0 as _);

        crate::asm::disable_irqs();

        let kernel_fs_base = read_thread_pointer();
        unsafe { write_thread_pointer(self.fs_base as _) };
        KernelGsBase::write(x86_64::VirtAddr::new_truncate(self.gs_base));

        unsafe { enter_user(self) };

        self.gs_base = KernelGsBase::read().as_u64();
        self.fs_base = read_thread_pointer() as _;
        unsafe { write_thread_pointer(kernel_fs_base) };

        let cr2 = Cr2::read().unwrap().as_u64() as usize;
        let vector = self.vector as u8;

        const PAGE_FAULT_VECTOR: u8 = ExceptionVector::Page as u8;

        let ret = match vector {
            PAGE_FAULT_VECTOR if let Ok(flags) = err_code_to_flags(self.error_code) => {
                ReturnReason::PageFault(va!(cr2), flags)
            }
            LEGACY_SYSCALL_VECTOR => ReturnReason::Syscall,
            IRQ_VECTOR_START..=IRQ_VECTOR_END => {
                handle_trap!(IRQ, vector as _);
                ReturnReason::Interrupt
            }
            _ => ReturnReason::Exception(ExceptionInfo {
                vector,
                error_code: self.error_code,
                cr2,
            }),
        };

        crate::asm::enable_irqs();
        ret
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
    /// Returns a generalized kind of this exception.
    pub fn kind(&self) -> ExceptionKind {
        match ExceptionVector::try_from(self.vector) {
            Ok(ExceptionVector::Breakpoint) => ExceptionKind::Breakpoint,
            Ok(ExceptionVector::InvalidOpcode) => ExceptionKind::IllegalInstruction,
            _ => ExceptionKind::Other,
        }
    }
}

/// Initializes syscall support and setups the syscall handler.
pub fn init_syscall() {
    extern "C" {
        fn syscall_entry();
    }

    LStar::write(x86_64::VirtAddr::new_truncate(syscall_entry as usize as _));
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
