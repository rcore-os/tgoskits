use core::arch::naked_asm;
use memory_addr::VirtAddr;
use riscv::register::sstatus::{self, FS};

/// General registers of RISC-V.
#[allow(missing_docs)]
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct GeneralRegisters {
    pub zero: usize,
    pub ra: usize,
    pub sp: usize,
    pub gp: usize, // only valid for user traps
    pub tp: usize, // only valid for user traps
    pub t0: usize,
    pub t1: usize,
    pub t2: usize,
    pub s0: usize,
    pub s1: usize,
    pub a0: usize,
    pub a1: usize,
    pub a2: usize,
    pub a3: usize,
    pub a4: usize,
    pub a5: usize,
    pub a6: usize,
    pub a7: usize,
    pub s2: usize,
    pub s3: usize,
    pub s4: usize,
    pub s5: usize,
    pub s6: usize,
    pub s7: usize,
    pub s8: usize,
    pub s9: usize,
    pub s10: usize,
    pub s11: usize,
    pub t3: usize,
    pub t4: usize,
    pub t5: usize,
    pub t6: usize,
}

/// Floating-point registers of RISC-V.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct FpState {
    /// the state of the RISC-V Floating-Point Unit (FPU)
    pub fp: [u64; 32],
    pub fcsr: usize,
    pub fs: FS,
}

impl Default for FpState {
    fn default() -> Self {
        Self {
            fs: FS::Initial,
            fp: [0; 32],
            fcsr: 0,
        }
    }
}

#[cfg(feature = "fp-simd")]
impl FpState {
    /// Restores the floating-point registers from this FP state
    #[inline]
    pub fn restore(&self) {
        unsafe { restore_fp_registers(self) }
    }

    /// Saves the current floating-point registers to this FP state
    #[inline]
    pub fn save(&mut self) {
        unsafe { save_fp_registers(self) }
    }

    /// Clears all floating-point registers to zero
    #[inline]
    pub fn clear() {
        unsafe { clear_fp_registers() }
    }

    /// Handles floating-point state context switching
    ///
    /// Saves the current task's FP state (if needed) and restores the next task's FP state
    pub fn switch_to(&mut self, next_fp_state: &FpState) {
        // get the real FP state of the current task
        let current_fs = sstatus::read().fs();
        // save the current task's FP state
        if current_fs == FS::Dirty {
            // we need to save the current task's FP state
            self.save();
            // after saving, we set the FP state to clean
            self.fs = FS::Clean;
        }
        // restore the next task's FP state
        match next_fp_state.fs {
            FS::Clean => next_fp_state.restore(), // the next task's FP state is clean, we should restore it
            FS::Initial => FpState::clear(),      // restore the FP state as constant values(all 0)
            FS::Off => {}                         // do nothing
            FS::Dirty => unreachable!("FP state of the next task should not be dirty"),
        }
        unsafe { sstatus::set_fs(next_fp_state.fs) }; // set the FP state to the next task's FP state
    }
}

/// Saved registers when a trap (interrupt or exception) occurs.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct TrapFrame {
    /// All general registers.
    pub regs: GeneralRegisters,
    /// Supervisor Exception Program Counter.
    pub sepc: usize,
    /// Supervisor Status Register.
    pub sstatus: sstatus::Sstatus,
}

impl Default for TrapFrame {
    fn default() -> Self {
        Self {
            regs: GeneralRegisters::default(),
            sepc: 0,
            sstatus: sstatus::Sstatus::from_bits(0),
        }
    }
}

impl TrapFrame {
    /// Gets the 0th syscall argument.
    pub const fn arg0(&self) -> usize {
        self.regs.a0
    }

    /// Gets the 1st syscall argument.
    pub const fn arg1(&self) -> usize {
        self.regs.a1
    }

    /// Gets the 2nd syscall argument.
    pub const fn arg2(&self) -> usize {
        self.regs.a2
    }

    /// Gets the 3rd syscall argument.
    pub const fn arg3(&self) -> usize {
        self.regs.a3
    }

    /// Gets the 4th syscall argument.
    pub const fn arg4(&self) -> usize {
        self.regs.a4
    }

    /// Gets the 5th syscall argument.
    pub const fn arg5(&self) -> usize {
        self.regs.a5
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
    pub ra: usize, // return address (x1)
    pub sp: usize, // stack pointer (x2)

    pub s0: usize, // x8-x9
    pub s1: usize,

    pub s2: usize, // x18-x27
    pub s3: usize,
    pub s4: usize,
    pub s5: usize,
    pub s6: usize,
    pub s7: usize,
    pub s8: usize,
    pub s9: usize,
    pub s10: usize,
    pub s11: usize,

    pub tp: usize,
    /// The `satp` register value, i.e., the page table root.
    #[cfg(feature = "uspace")]
    pub satp: memory_addr::PhysAddr,
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
        Self {
            #[cfg(feature = "uspace")]
            satp: crate::asm::read_kernel_page_table(),
            ..Default::default()
        }
    }

    /// Initializes the context for a new task, with the given entry point and
    /// kernel stack.
    pub fn init(&mut self, entry: usize, kstack_top: VirtAddr, tls_area: VirtAddr) {
        self.sp = kstack_top.as_usize();
        self.ra = entry;
        self.tp = tls_area.as_usize();
    }

    /// Changes the page table root in this context.
    ///
    /// The hardware register for page table root (`satp` for riscv64) will be
    /// updated to the next task's after [`Self::switch_to`].
    #[cfg(feature = "uspace")]
    pub fn set_page_table_root(&mut self, satp: memory_addr::PhysAddr) {
        self.satp = satp;
    }

    /// Switches to another task.
    ///
    /// It first saves the current task's context from CPU to this place, and then
    /// restores the next task's context from `next_ctx` to CPU.
    pub fn switch_to(&mut self, next_ctx: &Self) {
        #[cfg(feature = "tls")]
        {
            self.tp = crate::asm::read_thread_pointer();
            unsafe { crate::asm::write_thread_pointer(next_ctx.tp) };
        }
        #[cfg(feature = "uspace")]
        if self.satp != next_ctx.satp {
            unsafe { crate::asm::write_user_page_table(next_ctx.satp) };
            crate::asm::flush_tlb(None); // currently flush the entire TLB
        }
        #[cfg(feature = "fp-simd")]
        {
            self.fp_state.switch_to(&next_ctx.fp_state);
        }

        unsafe { context_switch(self, next_ctx) }
    }
}

#[cfg(feature = "fp-simd")]
#[unsafe(naked)]
unsafe extern "C" fn save_fp_registers(fp_state: &mut FpState) {
    naked_asm!(
        include_fp_asm_macros!(),
        "
        PUSH_FLOAT_REGS a0
        frcsr t0
        STR t0, a0, 32
        ret"
    )
}

#[cfg(feature = "fp-simd")]
#[unsafe(naked)]
unsafe extern "C" fn restore_fp_registers(fp_state: &FpState) {
    naked_asm!(
        include_fp_asm_macros!(),
        "
        POP_FLOAT_REGS a0
        LDR t0, a0, 32
        fscsr x0, t0
        ret"
    )
}

#[cfg(feature = "fp-simd")]
#[unsafe(naked)]
unsafe extern "C" fn clear_fp_registers() {
    naked_asm!(
        include_fp_asm_macros!(),
        "
        CLEAR_FLOAT_REGS
        ret"
    )
}

#[unsafe(naked)]
unsafe extern "C" fn context_switch(_current_task: &mut TaskContext, _next_task: &TaskContext) {
    naked_asm!(
        include_asm_macros!(),
        "
        // save old context (callee-saved registers)
        STR     ra, a0, 0
        STR     sp, a0, 1
        STR     s0, a0, 2
        STR     s1, a0, 3
        STR     s2, a0, 4
        STR     s3, a0, 5
        STR     s4, a0, 6
        STR     s5, a0, 7
        STR     s6, a0, 8
        STR     s7, a0, 9
        STR     s8, a0, 10
        STR     s9, a0, 11
        STR     s10, a0, 12
        STR     s11, a0, 13

        // restore new context
        LDR     s11, a1, 13
        LDR     s10, a1, 12
        LDR     s9, a1, 11
        LDR     s8, a1, 10
        LDR     s7, a1, 9
        LDR     s6, a1, 8
        LDR     s5, a1, 7
        LDR     s4, a1, 6
        LDR     s3, a1, 5
        LDR     s2, a1, 4
        LDR     s1, a1, 3
        LDR     s0, a1, 2
        LDR     sp, a1, 1
        LDR     ra, a1, 0

        ret",
    )
}
