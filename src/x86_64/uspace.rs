//! Structures and functions for user space.

use memory_addr::VirtAddr;

use crate::asm::{read_thread_pointer, write_thread_pointer};
use crate::TrapFrame;

/// Context to enter user space.
pub struct UspaceContext(TrapFrame);

impl UspaceContext {
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
    /// When an exception or syscall occurs, the kernel stack pointer is
    /// switched to `kstack_top`.
    ///
    /// # Safety
    ///
    /// This function is unsafe because it changes processor mode and the stack.
    pub unsafe fn enter_uspace(&self, kstack_top: VirtAddr) -> ! {
        crate::asm::disable_irqs();
        assert_eq!(super::gdt::read_tss_rsp0(), kstack_top);
        switch_to_user_fs_base(&self.0);
        unsafe {
            core::arch::asm!("
                mov     rsp, {tf}
                pop     rax
                pop     rcx
                pop     rdx
                pop     rbx
                pop     rbp
                pop     rsi
                pop     rdi
                pop     r8
                pop     r9
                pop     r10
                pop     r11
                pop     r12
                pop     r13
                pop     r14
                pop     r15
                add     rsp, 32     // skip fs_base, vector, error_code
                swapgs
                iretq",
                tf = in(reg) &self.0,
                options(noreturn),
            )
        }
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

impl core::ops::Deref for UspaceContext {
    type Target = TrapFrame;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl core::ops::DerefMut for UspaceContext {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}
