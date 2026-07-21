use core::{
    arch::naked_asm,
    mem::{align_of, offset_of, size_of},
    ptr::NonNull,
};

use ax_memory_addr::VirtAddr;
use cpu_local::CurrentThreadHeader;
use riscv::register::sstatus::{self, FS};

use crate::KernelTlsBase;

/// General registers of RISC-V.
#[allow(missing_docs)]
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct GeneralRegisters {
    pub zero: usize,
    pub ra: usize,
    pub sp: usize,
    pub gp: usize,
    pub tp: usize,
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
    /// the floating-point control and status register
    pub fcsr: usize,
    /// the floating-point status (dirty, clean, off)
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

        // FS gates every floating-point instruction. Bootstrap and kernel
        // contexts may legitimately leave it Off, so make register restore
        // legal before executing the first FP instruction. The task-owned
        // state is published only after the register image is complete.
        if matches!(next_fp_state.fs, FS::Clean | FS::Initial) {
            // SAFETY: the scheduler calls this handoff with IRQs disabled and
            // preemption pinned; sstatus changes only the current hart.
            unsafe { sstatus::set_fs(FS::Dirty) };
        }
        // restore the next task's FP state
        match next_fp_state.fs {
            FS::Clean => next_fp_state.restore(), /* the next task's FP state is clean, we should restore it */
            FS::Initial => FpState::clear(),      // restore the FP state as constant values(all 0)
            FS::Off => {}                         // do nothing
            FS::Dirty => unreachable!("FP state of the next task should not be dirty"),
        }
        // SAFETY: the same IRQ-disabled, CPU-pinned handoff still owns this
        // hart, and every FS variant is a valid architectural encoding.
        unsafe { sstatus::set_fs(next_fp_state.fs) };
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
    /// Returns the privilege domain represented by this register image.
    pub fn origin(&self) -> crate::TrapOrigin {
        match self.sstatus.spp() {
            sstatus::SPP::Supervisor => crate::TrapOrigin::Kernel,
            sstatus::SPP::User => crate::TrapOrigin::User,
        }
    }

    /// Gets the 0th syscall argument.
    pub const fn arg0(&self) -> usize {
        self.regs.a0
    }

    /// Sets the 0th syscall argument.
    pub const fn set_arg0(&mut self, a0: usize) {
        self.regs.a0 = a0;
    }

    /// Gets the 1st syscall argument.
    pub const fn arg1(&self) -> usize {
        self.regs.a1
    }

    /// Sets the 1th syscall argument.
    pub const fn set_arg1(&mut self, a1: usize) {
        self.regs.a1 = a1;
    }

    /// Gets the 2nd syscall argument.
    pub const fn arg2(&self) -> usize {
        self.regs.a2
    }

    /// Sets the 2nd syscall argument.
    pub const fn set_arg2(&mut self, a2: usize) {
        self.regs.a2 = a2;
    }

    /// Gets the 3rd syscall argument.
    pub const fn arg3(&self) -> usize {
        self.regs.a3
    }

    /// Sets the 3rd syscall argument.
    pub const fn set_arg3(&mut self, a3: usize) {
        self.regs.a3 = a3;
    }

    /// Gets the 4th syscall argument.
    pub const fn arg4(&self) -> usize {
        self.regs.a4
    }

    /// Sets the 4th syscall argument.
    pub const fn set_arg4(&mut self, a4: usize) {
        self.regs.a4 = a4;
    }

    /// Gets the 5th syscall argument.
    pub const fn arg5(&self) -> usize {
        self.regs.a5
    }

    /// Sets the 5th syscall argument.
    pub const fn set_arg5(&mut self, a5: usize) {
        self.regs.a5 = a5;
    }

    /// Gets the syscall number.
    pub const fn sysno(&self) -> usize {
        self.regs.a7
    }

    /// Sets the syscall number.
    pub const fn set_sysno(&mut self, a7: usize) {
        self.regs.a7 = a7;
    }

    /// Gets the instruction pointer.
    pub const fn ip(&self) -> usize {
        self.sepc
    }

    /// Sets the instruction pointer.
    pub const fn set_ip(&mut self, pc: usize) {
        self.sepc = pc;
    }

    /// Gets the stack pointer.
    pub const fn sp(&self) -> usize {
        self.regs.sp
    }

    /// Sets the stack pointer.
    pub const fn set_sp(&mut self, sp: usize) {
        self.regs.sp = sp;
    }

    /// Gets the return value register.
    pub const fn retval(&self) -> usize {
        self.regs.a0
    }

    /// Sets the return value register.
    pub const fn set_retval(&mut self, a0: usize) {
        self.regs.a0 = a0;
    }

    /// Sets the return address.
    pub const fn set_ra(&mut self, ra: usize) {
        self.regs.ra = ra;
    }

    /// Gets the TLS area.
    pub const fn tls(&self) -> usize {
        self.regs.tp
    }

    /// Sets the TLS area.
    pub const fn set_tls(&mut self, tls_area: usize) {
        self.regs.tp = tls_area;
    }

    /// Unwind the stack and get the backtrace.
    pub fn backtrace(&self) -> axbacktrace::Backtrace {
        axbacktrace::Backtrace::capture_trap(self.regs.s0 as _, self.sepc as _, self.regs.ra as _)
    }
}

/// Saved hardware states of a task.
///
/// The context usually includes:
///
/// - Callee-saved registers
/// - Stack pointer register
/// - Thread pointer register (for kernel-space thread-local storage)
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
    /// Pinned task-owned header loaded into `tp` by LinuxCurrent images.
    current_header: usize,
    /// Kernel task-local storage pointer loaded into `tp` by UnikernelTls.
    kernel_tls: KernelTlsBase,
    /// The `satp` value restored for this task's userspace address space.
    #[cfg(feature = "uspace")]
    page_table_root: ax_memory_addr::PhysAddr,
    #[cfg(feature = "fp-simd")]
    pub fp_state: FpState,
}

// RISC-V load/store macros accept a machine-word index. Derive every index
// from this C layout and prove the TLS newtype has exactly one register word.
const _: () = {
    assert!(size_of::<KernelTlsBase>() == size_of::<usize>());
    assert!(align_of::<KernelTlsBase>() == align_of::<usize>());
    assert!(offset_of!(TaskContext, ra) == 0);
    assert!(offset_of!(TaskContext, sp) == offset_of!(TaskContext, ra) + size_of::<usize>());
    assert!(offset_of!(TaskContext, current_header) % size_of::<usize>() == 0);
    assert!(
        offset_of!(TaskContext, kernel_tls)
            == offset_of!(TaskContext, current_header) + size_of::<usize>()
    );
};

impl TaskContext {
    /// Creates a dummy context for a new task.
    ///
    /// Note the context is not initialized, it will be filled by
    /// [`switch_to_raw`](Self::switch_to_raw) (for initial tasks) and [`init`]
    /// (for regular tasks) methods.
    ///
    /// [`init`]: TaskContext::init
    pub fn new() -> Self {
        Self {
            #[cfg(feature = "uspace")]
            page_table_root: crate::asm::read_kernel_page_table(),
            ..Self::default()
        }
    }

    /// Initializes the context for a new task, with the given entry point and
    /// kernel stack.
    pub fn init(&mut self, entry: usize, kstack_top: VirtAddr, tls_area: KernelTlsBase) {
        self.sp = kstack_top.as_usize();
        self.ra = entry;
        self.kernel_tls = KernelTlsBase::for_task_context(tls_area);
    }

    /// Sets the pinned task-owned current-thread header.
    pub fn set_current_header(&mut self, header: NonNull<CurrentThreadHeader>) {
        self.current_header = header.as_ptr() as usize;
    }

    /// Returns the configured task-owned current-thread header.
    pub const fn current_header(&self) -> Option<NonNull<CurrentThreadHeader>> {
        NonNull::new(self.current_header as *mut CurrentThreadHeader)
    }

    /// Changes the page table root restored for this task.
    #[cfg(feature = "uspace")]
    pub fn set_page_table_root(&mut self, page_table_root: ax_memory_addr::PhysAddr) {
        self.page_table_root = page_table_root;
    }

    /// Completes FP/SIMD work before current-thread publication.
    pub fn prepare_switch_to(&mut self, _next_ctx: &Self) {
        #[cfg(feature = "fp-simd")]
        {
            self.fp_state.switch_to(&_next_ctx.fp_state);
        }
        #[cfg(feature = "uspace")]
        if self.page_table_root != _next_ctx.page_table_root {
            // SAFETY: the scheduler owns both contexts with IRQs disabled.
            unsafe { crate::asm::write_user_page_table(_next_ctx.page_table_root) };
            crate::asm::flush_tlb(None);
        }
    }

    /// Performs only the final GPR/current/TLS transfer.
    ///
    /// # Safety
    ///
    /// Scheduling must be serialized, FP state prepared, and the next current
    /// header published. No fallible Rust work may follow before this call.
    #[inline(always)]
    pub unsafe fn switch_to_raw(&mut self, next_ctx: &Self) {
        unsafe { context_switch_raw(self, next_ctx) }
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

#[cfg(feature = "tls")]
#[unsafe(naked)]
unsafe extern "C" fn context_switch_raw(_current_task: &mut TaskContext, _next_task: &TaskContext) {
    naked_asm!(
        include_asm_macros!(),
        "
        // save old context (callee-saved registers)
        STR     ra, a0, {ra_index}
        STR     sp, a0, {sp_index}
        STR     s0, a0, {s0_index}
        STR     s1, a0, {s1_index}
        STR     s2, a0, {s2_index}
        STR     s3, a0, {s3_index}
        STR     s4, a0, {s4_index}
        STR     s5, a0, {s5_index}
        STR     s6, a0, {s6_index}
        STR     s7, a0, {s7_index}
        STR     s8, a0, {s8_index}
        STR     s9, a0, {s9_index}
        STR     s10, a0, {s10_index}
        STR     s11, a0, {s11_index}
        STR     tp, a0, {kernel_tls_index}

        // restore new context
        LDR     s11, a1, {s11_index}
        LDR     s10, a1, {s10_index}
        LDR     s9, a1, {s9_index}
        LDR     s8, a1, {s8_index}
        LDR     s7, a1, {s7_index}
        LDR     s6, a1, {s6_index}
        LDR     s5, a1, {s5_index}
        LDR     s4, a1, {s4_index}
        LDR     s3, a1, {s3_index}
        LDR     s2, a1, {s2_index}
        LDR     s1, a1, {s1_index}
        LDR     s0, a1, {s0_index}
        LDR     sp, a1, {sp_index}
        LDR     tp, a1, {kernel_tls_index}
        LDR     ra, a1, {ra_index}

        ret",
        ra_index = const offset_of!(TaskContext, ra) / size_of::<usize>(),
        sp_index = const offset_of!(TaskContext, sp) / size_of::<usize>(),
        s0_index = const offset_of!(TaskContext, s0) / size_of::<usize>(),
        s1_index = const offset_of!(TaskContext, s1) / size_of::<usize>(),
        s2_index = const offset_of!(TaskContext, s2) / size_of::<usize>(),
        s3_index = const offset_of!(TaskContext, s3) / size_of::<usize>(),
        s4_index = const offset_of!(TaskContext, s4) / size_of::<usize>(),
        s5_index = const offset_of!(TaskContext, s5) / size_of::<usize>(),
        s6_index = const offset_of!(TaskContext, s6) / size_of::<usize>(),
        s7_index = const offset_of!(TaskContext, s7) / size_of::<usize>(),
        s8_index = const offset_of!(TaskContext, s8) / size_of::<usize>(),
        s9_index = const offset_of!(TaskContext, s9) / size_of::<usize>(),
        s10_index = const offset_of!(TaskContext, s10) / size_of::<usize>(),
        s11_index = const offset_of!(TaskContext, s11) / size_of::<usize>(),
        kernel_tls_index = const offset_of!(TaskContext, kernel_tls) / size_of::<usize>(),
    )
}

#[cfg(not(feature = "tls"))]
#[unsafe(naked)]
unsafe extern "C" fn context_switch_raw(_current_task: &mut TaskContext, _next_task: &TaskContext) {
    naked_asm!(
        include_asm_macros!(),
        "
        // Save old context. CPU-owned sscratch and task-owned tp are not
        // folded into the generic callee-saved register image.
        STR     ra, a0, {ra_index}
        STR     sp, a0, {sp_index}
        STR     s0, a0, {s0_index}
        STR     s1, a0, {s1_index}
        STR     s2, a0, {s2_index}
        STR     s3, a0, {s3_index}
        STR     s4, a0, {s4_index}
        STR     s5, a0, {s5_index}
        STR     s6, a0, {s6_index}
        STR     s7, a0, {s7_index}
        STR     s8, a0, {s8_index}
        STR     s9, a0, {s9_index}
        STR     s10, a0, {s10_index}
        STR     s11, a0, {s11_index}

        // Restore all next state, then make tp current immediately before the
        // direct return into the next task. gp remains the psABI global pointer.
        LDR     s11, a1, {s11_index}
        LDR     s10, a1, {s10_index}
        LDR     s9, a1, {s9_index}
        LDR     s8, a1, {s8_index}
        LDR     s7, a1, {s7_index}
        LDR     s6, a1, {s6_index}
        LDR     s5, a1, {s5_index}
        LDR     s4, a1, {s4_index}
        LDR     s3, a1, {s3_index}
        LDR     s2, a1, {s2_index}
        LDR     s1, a1, {s1_index}
        LDR     s0, a1, {s0_index}
        LDR     sp, a1, {sp_index}
        LDR     tp, a1, {current_header_index}
        LDR     ra, a1, {ra_index}
        ret",
        ra_index = const offset_of!(TaskContext, ra) / size_of::<usize>(),
        sp_index = const offset_of!(TaskContext, sp) / size_of::<usize>(),
        s0_index = const offset_of!(TaskContext, s0) / size_of::<usize>(),
        s1_index = const offset_of!(TaskContext, s1) / size_of::<usize>(),
        s2_index = const offset_of!(TaskContext, s2) / size_of::<usize>(),
        s3_index = const offset_of!(TaskContext, s3) / size_of::<usize>(),
        s4_index = const offset_of!(TaskContext, s4) / size_of::<usize>(),
        s5_index = const offset_of!(TaskContext, s5) / size_of::<usize>(),
        s6_index = const offset_of!(TaskContext, s6) / size_of::<usize>(),
        s7_index = const offset_of!(TaskContext, s7) / size_of::<usize>(),
        s8_index = const offset_of!(TaskContext, s8) / size_of::<usize>(),
        s9_index = const offset_of!(TaskContext, s9) / size_of::<usize>(),
        s10_index = const offset_of!(TaskContext, s10) / size_of::<usize>(),
        s11_index = const offset_of!(TaskContext, s11) / size_of::<usize>(),
        current_header_index = const offset_of!(TaskContext, current_header) / size_of::<usize>(),
    )
}
