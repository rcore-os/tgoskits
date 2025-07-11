//! Structures and functions for user space.

use memory_addr::VirtAddr;

use crate::TrapFrame;

/// Context to enter user space.
pub struct UspaceContext(TrapFrame);

impl UspaceContext {
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

    /// Creates a new context from the given [`TrapFrame`].
    pub const fn from(trap_frame: &TrapFrame) -> Self {
        Self(*trap_frame)
    }

    /// Enters user space.
    ///
    /// It restores the user registers and jumps to the user entry point
    /// (saved in `era`).
    /// When an exception or syscall occurs, the kernel stack pointer is
    /// switched to `kstack_top`.
    ///
    /// # Safety
    ///
    /// This function is unsafe because it changes processor mode and the stack.
    pub unsafe fn enter_uspace(&self, kstack_top: VirtAddr) -> ! {
        use loongArch64::register::era;

        crate::asm::disable_irqs();
        crate::asm::write_kernel_sp(kstack_top.as_usize());
        era::set_pc(self.ip());

        unsafe {
            core::arch::asm!(
                include_asm_macros!(),
                "
                move      $sp, {tf}
                csrwr     $tp,  KSAVE_TP
                csrwr     $r21, KSAVE_R21
                LDD       $tp,  $sp, 32
                csrwr     $tp,  LA_CSR_PRMD

                POP_GENERAL_REGS

                LDD      $tp,   $sp, 2
                LDD      $r21,  $sp, 21
                LDD      $sp,   $sp, 3       // user sp
                ertn",
                tf = in (reg) &self.0,
                options(noreturn),
            )
        }
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
