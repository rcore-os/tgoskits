use core::arch::naked_asm;
use core::fmt;
use memory_addr::VirtAddr;

/// Saved registers when a trap (exception) occurs.
#[repr(C)]
#[derive(Default, Clone, Copy)]
pub struct TrapFrame {
    /// General-purpose registers (R0..R30).
    pub r: [u64; 31],
    /// User Stack Pointer (SP_EL0).
    pub usp: u64,
    /// Exception Link Register (ELR_EL1).
    pub elr: u64,
    /// Saved Process Status Register (SPSR_EL1).
    pub spsr: u64,
}

impl fmt::Debug for TrapFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "TrapFrame: {{")?;
        for (i, &reg) in self.r.iter().enumerate() {
            writeln!(f, "    r{i}: {reg:#x},")?;
        }
        writeln!(f, "    usp: {:#x},", self.usp)?;
        writeln!(f, "    elr: {:#x},", self.elr)?;
        writeln!(f, "    spsr: {:#x},", self.spsr)?;
        write!(f, "}}")?;
        Ok(())
    }
}

impl TrapFrame {
    /// Gets the 0th syscall argument.
    pub const fn arg0(&self) -> usize {
        self.r[0] as _
    }

    /// Gets the 1st syscall argument.
    pub const fn arg1(&self) -> usize {
        self.r[1] as _
    }

    /// Gets the 2nd syscall argument.
    pub const fn arg2(&self) -> usize {
        self.r[2] as _
    }

    /// Gets the 3rd syscall argument.
    pub const fn arg3(&self) -> usize {
        self.r[3] as _
    }

    /// Gets the 4th syscall argument.
    pub const fn arg4(&self) -> usize {
        self.r[4] as _
    }

    /// Gets the 5th syscall argument.
    pub const fn arg5(&self) -> usize {
        self.r[5] as _
    }
}

/// FP & SIMD registers.
#[repr(C, align(16))]
#[derive(Debug, Default)]
pub struct FpState {
    /// 128-bit SIMD & FP registers (V0..V31)
    pub regs: [u128; 32],
    /// Floating-point Control Register (FPCR)
    pub fpcr: u32,
    /// Floating-point Status Register (FPSR)
    pub fpsr: u32,
}

#[cfg(feature = "fp-simd")]
impl FpState {
    /// Saves the current FP/SIMD states from CPU to this structure.
    pub fn save(&mut self) {
        unsafe { fpstate_save(self) }
    }

    /// Restores the FP/SIMD states from this structure to CPU.
    pub fn restore(&self) {
        unsafe { fpstate_restore(self) }
    }
}

/// Saved hardware states of a task.
///
/// The context usually includes:
///
/// - Callee-saved registers
/// - Stack pointer register
/// - Thread pointer register (for thread-local storage, currently unsupported)
/// - FP/SIMD registers
///
/// On context switch, current task saves its context from CPU to memory,
/// and the next task restores its context from memory to CPU.
#[allow(missing_docs)]
#[repr(C)]
#[derive(Debug, Default)]
pub struct TaskContext {
    pub sp: u64,
    pub tpidr_el0: u64,
    pub r19: u64,
    pub r20: u64,
    pub r21: u64,
    pub r22: u64,
    pub r23: u64,
    pub r24: u64,
    pub r25: u64,
    pub r26: u64,
    pub r27: u64,
    pub r28: u64,
    pub r29: u64,
    pub lr: u64, // r30
    /// The `ttbr0_el1` register value, i.e., the page table root.
    #[cfg(feature = "uspace")]
    pub ttbr0_el1: memory_addr::PhysAddr,
    #[cfg(feature = "fp-simd")]
    pub fp_state: FpState,
}

impl TaskContext {
    /// Creates a dummy context for a new task.
    ///
    /// Note the context is not initialized, it will be filled by [`switch_to`]
    /// (for initial tasks) and [`init`] (for regular tasks) methods.
    ///
    /// [`init`]: TaskContext::init
    /// [`switch_to`]: TaskContext::switch_to
    pub fn new() -> Self {
        Self::default()
    }

    /// Initializes the context for a new task, with the given entry point and
    /// kernel stack.
    pub fn init(&mut self, entry: usize, kstack_top: VirtAddr, tls_area: VirtAddr) {
        self.sp = kstack_top.as_usize() as u64;
        self.lr = entry as u64;
        // When under `uspace` feature, kernel will not use this register.
        self.tpidr_el0 = tls_area.as_usize() as u64;
    }

    /// Changes the page table root in this context.
    ///
    /// The hardware register for user page table root (`ttbr0_el1` for aarch64 in EL1)
    /// will be updated to the next task's after [`Self::switch_to`].
    #[cfg(feature = "uspace")]
    pub fn set_page_table_root(&mut self, ttbr0_el1: memory_addr::PhysAddr) {
        self.ttbr0_el1 = ttbr0_el1;
    }

    /// Switches to another task.
    ///
    /// It first saves the current task's context from CPU to this place, and then
    /// restores the next task's context from `next_ctx` to CPU.
    pub fn switch_to(&mut self, next_ctx: &Self) {
        #[cfg(feature = "fp-simd")]
        {
            self.fp_state.save();
            next_ctx.fp_state.restore();
        }
        #[cfg(feature = "uspace")]
        if self.ttbr0_el1 != next_ctx.ttbr0_el1 {
            unsafe { crate::asm::write_user_page_table(next_ctx.ttbr0_el1) };
            crate::asm::flush_tlb(None); // currently flush the entire TLB
        }
        unsafe { context_switch(self, next_ctx) }
    }
}

#[unsafe(naked)]
unsafe extern "C" fn context_switch(_current_task: &mut TaskContext, _next_task: &TaskContext) {
    naked_asm!(
        "
        // save old context (callee-saved registers)
        stp     x29, x30, [x0, 12 * 8]
        stp     x27, x28, [x0, 10 * 8]
        stp     x25, x26, [x0, 8 * 8]
        stp     x23, x24, [x0, 6 * 8]
        stp     x21, x22, [x0, 4 * 8]
        stp     x19, x20, [x0, 2 * 8]
        mov     x19, sp
        mrs     x20, tpidr_el0
        stp     x19, x20, [x0]

        // restore new context
        ldp     x19, x20, [x1]
        mov     sp, x19
        msr     tpidr_el0, x20
        ldp     x19, x20, [x1, 2 * 8]
        ldp     x21, x22, [x1, 4 * 8]
        ldp     x23, x24, [x1, 6 * 8]
        ldp     x25, x26, [x1, 8 * 8]
        ldp     x27, x28, [x1, 10 * 8]
        ldp     x29, x30, [x1, 12 * 8]

        ret",
    )
}

#[unsafe(naked)]
#[cfg(feature = "fp-simd")]
unsafe extern "C" fn fpstate_save(state: &mut FpState) {
    naked_asm!(
        ".arch armv8
        // save fp/neon context
        mrs     x9, fpcr
        mrs     x10, fpsr
        stp     q0, q1, [x0, 0 * 16]
        stp     q2, q3, [x0, 2 * 16]
        stp     q4, q5, [x0, 4 * 16]
        stp     q6, q7, [x0, 6 * 16]
        stp     q8, q9, [x0, 8 * 16]
        stp     q10, q11, [x0, 10 * 16]
        stp     q12, q13, [x0, 12 * 16]
        stp     q14, q15, [x0, 14 * 16]
        stp     q16, q17, [x0, 16 * 16]
        stp     q18, q19, [x0, 18 * 16]
        stp     q20, q21, [x0, 20 * 16]
        stp     q22, q23, [x0, 22 * 16]
        stp     q24, q25, [x0, 24 * 16]
        stp     q26, q27, [x0, 26 * 16]
        stp     q28, q29, [x0, 28 * 16]
        stp     q30, q31, [x0, 30 * 16]
        str     x9, [x0, 64 *  8]
        str     x10, [x0, 65 * 8]

        isb
        ret"
    )
}

#[unsafe(naked)]
#[cfg(feature = "fp-simd")]
unsafe extern "C" fn fpstate_restore(state: &FpState) {
    naked_asm!(
        ".arch armv8
        // restore fp/neon context
        ldp     q0, q1, [x0, 0 * 16]
        ldp     q2, q3, [x0, 2 * 16]
        ldp     q4, q5, [x0, 4 * 16]
        ldp     q6, q7, [x0, 6 * 16]
        ldp     q8, q9, [x0, 8 * 16]
        ldp     q10, q11, [x0, 10 * 16]
        ldp     q12, q13, [x0, 12 * 16]
        ldp     q14, q15, [x0, 14 * 16]
        ldp     q16, q17, [x0, 16 * 16]
        ldp     q18, q19, [x0, 18 * 16]
        ldp     q20, q21, [x0, 20 * 16]
        ldp     q22, q23, [x0, 22 * 16]
        ldp     q24, q25, [x0, 24 * 16]
        ldp     q26, q27, [x0, 26 * 16]
        ldp     q28, q29, [x0, 28 * 16]
        ldp     q30, q31, [x0, 30 * 16]
        ldr     x9, [x0, 64 * 8]
        ldr     x10, [x0, 65 * 8]
        msr     fpcr, x9
        msr     fpsr, x10

        isb
        ret"
    )
}
