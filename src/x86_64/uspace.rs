//! Structures and functions for user space.

use core::ops::{Deref, DerefMut};

use memory_addr::VirtAddr;

use crate::asm::{read_thread_pointer, write_thread_pointer};
use crate::trap::{ExceptionKind, ReturnReason};
use crate::TrapFrame;

/// Context to enter user space.
pub struct UserContext(TrapFrame);

impl UserContext {
    /// Creates an empty context with all registers set to zero.
    pub const fn empty() -> Self {
        unsafe { core::mem::MaybeUninit::zeroed().assume_init() }
    }

    /// Creates a new context with the given entry point, user stack pointer,
    /// and the argument.
    pub fn new(entry: usize, ustack_top: VirtAddr, arg0: usize) -> Self {
        use crate::GdtStruct;
        use x86_64::registers::rflags::RFlags;
        Self(TrapFrame {
            rdi: arg0 as _,
            rip: entry as _,
            cs: GdtStruct::UCODE64_SELECTOR.0 as _,
            rflags: RFlags::INTERRUPT_FLAG.bits(), // IOPL = 0, IF = 1
            rsp: ustack_top.as_usize() as _,
            ss: GdtStruct::UDATA_SELECTOR.0 as _,
            ..Default::default()
        })
    }

    /// Creates a new context from the given [`TrapFrame`].
    ///
    /// It copies almost all registers except `CS` and `SS` which need to be
    /// set to the user segment selectors.
    pub const fn from(tf: &TrapFrame) -> Self {
        use crate::GdtStruct;
        let mut tf = *tf;
        tf.cs = GdtStruct::UCODE64_SELECTOR.0 as _;
        tf.ss = GdtStruct::UDATA_SELECTOR.0 as _;
        Self(tf)
    }

    /// Enters user space.
    ///
    /// It restores the user registers and jumps to the user entry point
    /// (saved in `rip`).
    ///
    /// This function returns when an exception or syscall occurs.
    pub fn run(&mut self) -> ReturnReason {
        // TODO: implement
        ReturnReason::Unknown
    }
}

// TLS support functions
#[cfg(feature = "tls")]
#[percpu::def_percpu]
static KERNEL_FS_BASE: usize = 0;

/// Switches to kernel FS base for TLS support.
pub fn switch_to_kernel_fs_base(tf: &mut TrapFrame) {
    if tf.is_user() {
        tf.fs_base = read_thread_pointer() as _;
        #[cfg(feature = "tls")]
        unsafe {
            write_thread_pointer(KERNEL_FS_BASE.read_current())
        };
    }
}

/// Switches to user FS base for TLS support.
pub fn switch_to_user_fs_base(tf: &TrapFrame) {
    if tf.is_user() {
        #[cfg(feature = "tls")]
        KERNEL_FS_BASE.write_current(read_thread_pointer());
        unsafe { write_thread_pointer(tf.fs_base as _) };
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

#[derive(Debug, Clone, Copy)]
pub struct ExceptionInfo {}

impl ExceptionInfo {
    pub fn kind(&self) -> ExceptionKind {
        // TODO: implement
        ExceptionKind::Other
    }
}
