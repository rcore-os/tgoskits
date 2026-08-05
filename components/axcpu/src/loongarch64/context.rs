use core::{
    arch::naked_asm,
    mem::{align_of, offset_of, size_of},
    ptr::NonNull,
};

use ax_memory_addr::VirtAddr;
use cpu_local::{CurrentThreadHeader, PreparedThreadSwitch};

use crate::{KernelTlsBase, TaskLocalState};

/// General registers of Loongarch64.
#[allow(missing_docs)]
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct GeneralRegisters {
    pub zero: usize,
    pub ra: usize,
    pub tp: usize,
    pub sp: usize,
    pub a0: usize,
    pub a1: usize,
    pub a2: usize,
    pub a3: usize,
    pub a4: usize,
    pub a5: usize,
    pub a6: usize,
    pub a7: usize,
    pub t0: usize,
    pub t1: usize,
    pub t2: usize,
    pub t3: usize,
    pub t4: usize,
    pub t5: usize,
    pub t6: usize,
    pub t7: usize,
    pub t8: usize,
    /// User `u0` on a user-origin trap; a diagnostic per-CPU snapshot on a
    /// kernel-origin trap. Kernel return deliberately ignores this field.
    pub u0: usize,
    pub fp: usize,
    pub s0: usize,
    pub s1: usize,
    pub s2: usize,
    pub s3: usize,
    pub s4: usize,
    pub s5: usize,
    pub s6: usize,
    pub s7: usize,
    pub s8: usize,
}

/// Floating-point / LSX/LASX vector registers of LoongArch64.
///
/// The platform enables the LSX 128-bit and LASX 256-bit vector extensions at
/// boot, so user code may use the full 256-bit vector registers `xr0`-`xr31`.
/// The scalar FP registers `f0`-`f31` alias the low 64 bits of `vr0`-`vr31`,
/// and `vr0`-`vr31` alias the low 128 bits of `xr0`-`xr31`. To correctly
/// save/restore the live FP/vector state across a context switch or a signal
/// delivery we must preserve all 256 bits of each register, not just the scalar
/// low 64 bits.
///
/// `fp` holds the low 64 bits (`xr[63:0]`, i.e. the scalar `fN` view),
/// `fp_high` holds `xr[127:64]`, and the LASX-only fields hold the upper
/// `xr[255:128]`. Keeping `fp` first and with its original layout means any
/// consumer that treated `fp[i]` as the scalar double `fN` still observes the
/// same value; the vector extension fields are additive.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct FpuState {
    /// Low 64 bits of the vector registers `vr0`-`vr31` (the scalar `f0`-`f31`).
    pub fp: [u64; 32],
    /// High 64 bits of the vector registers `vr0`-`vr31` (`vr[127:64]`).
    ///
    /// Saved/restored via LSX so an async signal (or preemptive switch)
    /// interrupting a thread mid-LSX-op does not clobber these on resume.
    pub fp_high: [u64; 32],
    /// LASX-only doubleword element 2 (`xr[191:128]`) of `xr0`-`xr31`.
    pub fp_lasx_hi0: [u64; 32],
    /// LASX-only doubleword element 3 (`xr[255:192]`) of `xr0`-`xr31`.
    pub fp_lasx_hi1: [u64; 32],
    /// Floating-point Condition Code register
    pub fcc: [u8; 8],
    /// Floating-point Control and Status register
    pub fcsr: u32,
}

#[cfg(feature = "fp-simd")]
impl FpuState {
    /// Save the current FPU states from CPU to this structure.
    #[inline]
    pub fn save(&mut self) {
        unsafe { save_fp_registers(self) }
    }

    /// Restore FPU states from this structure to CPU.
    #[inline]
    pub fn restore(&self) {
        unsafe { restore_fp_registers(self) }
    }
}

/// Saved registers when a trap (interrupt or exception) occurs.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct TrapFrame {
    /// All general registers.
    pub regs: GeneralRegisters,
    /// Pre-exception Mode Information
    pub prmd: usize,
    /// Exception Return Address
    pub era: usize,
}

impl TrapFrame {
    /// Returns whether the saved register image belongs to kernel or user
    /// execution.
    ///
    /// In particular, `regs.u0` is restorable user state only for
    /// [`crate::TrapOrigin::User`].
    pub const fn origin(&self) -> crate::TrapOrigin {
        if self.prmd & 0b11 == 0 {
            crate::TrapOrigin::Kernel
        } else {
            crate::TrapOrigin::User
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

    /// Sets the 1st syscall argument.
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

    /// Get the syscall number.
    pub const fn sysno(&self) -> usize {
        self.regs.a7
    }

    /// Sets the syscall number.
    pub const fn set_sysno(&mut self, a7: usize) {
        self.regs.a7 = a7;
    }

    /// Gets the instruction pointer.
    pub const fn ip(&self) -> usize {
        self.era
    }

    /// Sets the instruction pointer.
    pub const fn set_ip(&mut self, pc: usize) {
        self.era = pc;
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
        axbacktrace::Backtrace::capture_trap(self.regs.fp as _, self.era as _, self.regs.ra as _)
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
#[derive(Debug)]
pub struct TaskContext {
    /// Return Address
    pub ra: usize,
    /// Stack Pointer
    pub sp: usize,
    /// loongArch need to save 10 static registers from $r22 to $r31
    pub s: [usize; 10],
    /// Architecture-neutral current-header and kernel-TLS switch state.
    task_local: TaskLocalState,
    /// The `PGDL` value restored for this task's userspace address space.
    #[cfg(feature = "uspace")]
    page_table_root: usize,
    #[cfg(feature = "fp-simd")]
    /// Floating Point Unit states
    pub fpu: FpuState,
}

// The naked switch uses one machine-word load/store for each field. Keep the
// array packing and TLS representation assumptions checked by the compiler.
const _: () = {
    assert!(size_of::<KernelTlsBase>() == size_of::<usize>());
    assert!(align_of::<KernelTlsBase>() == align_of::<usize>());
    assert!(offset_of!(TaskContext, ra) == 0);
    assert!(offset_of!(TaskContext, sp) == offset_of!(TaskContext, ra) + size_of::<usize>());
    assert!(size_of::<[usize; 10]>() == 10 * size_of::<usize>());
    assert!(
        offset_of!(TaskContext, task_local)
            == offset_of!(TaskContext, s) + size_of::<[usize; 10]>()
    );
};

impl Default for TaskContext {
    fn default() -> Self {
        Self {
            ra: 0,
            sp: 0,
            s: [0; 10],
            task_local: TaskLocalState::new(),
            #[cfg(feature = "uspace")]
            page_table_root: 0,
            #[cfg(feature = "fp-simd")]
            fpu: FpuState::default(),
        }
    }
}

impl TaskContext {
    /// Creates a new default context for a new task.
    pub fn new() -> Self {
        Self {
            #[cfg(feature = "uspace")]
            page_table_root: crate::asm::read_user_page_table().as_usize(),
            ..Self::default()
        }
    }

    /// Initializes a task context with its entry point, kernel stack, and
    /// task-owned kernel TLS base.
    pub fn init(&mut self, entry: usize, kstack_top: VirtAddr, kernel_tls: KernelTlsBase) {
        self.sp = kstack_top.as_usize();
        self.ra = entry;
        self.task_local.set_kernel_tls(kernel_tls);
    }

    /// Sets the pinned task-owned current-thread header.
    pub fn set_current_header(&mut self, header: NonNull<CurrentThreadHeader>) {
        self.task_local.set_current_header(header);
    }

    /// Returns the configured task-owned current-thread header.
    pub const fn current_header(&self) -> Option<NonNull<CurrentThreadHeader>> {
        self.task_local.current_header()
    }

    /// Changes the page table root restored for this task.
    #[cfg(feature = "uspace")]
    pub fn set_page_table_root(&mut self, page_table_root: ax_memory_addr::PhysAddr) {
        self.page_table_root = page_table_root.as_usize();
    }

    /// Completes FPU work before current-thread publication.
    pub fn prepare_switch_to(&mut self, _next_ctx: &Self) {
        #[cfg(feature = "fp-simd")]
        {
            self.fpu.save();
            _next_ctx.fpu.restore();
        }
        #[cfg(feature = "uspace")]
        if self.page_table_root != _next_ctx.page_table_root {
            // SAFETY: the scheduler owns both contexts with IRQs disabled.
            unsafe {
                crate::asm::write_user_page_table(ax_memory_addr::PhysAddr::from(
                    _next_ctx.page_table_root,
                ))
            };
            crate::asm::flush_tlb(None);
        }
    }

    /// Performs only the final GPR/current/TLS transfer.
    ///
    /// # Safety
    ///
    /// Scheduling must be serialized, FPU state prepared, and the next current
    /// header published. No fallible Rust work may follow before this call.
    #[inline(always)]
    pub unsafe fn switch_to_prepared(
        &mut self,
        next_ctx: &Self,
        prepared: PreparedThreadSwitch<'_>,
    ) {
        assert_eq!(
            next_ctx.current_header(),
            Some(prepared.next_header()),
            "prepared switch token must belong to the next task context",
        );
        unsafe { prepared.commit() };
        unsafe { context_switch_raw(self, next_ctx) }
    }
}

#[cfg(feature = "fp-simd")]
#[unsafe(naked)]
unsafe extern "C" fn save_fp_registers(fpu: &mut FpuState) {
    naked_asm!(
        include_fp_asm_macros!(),
        "
        SAVE_FP $a0
        addi.d $t8, $a0, {fp_high_offset}
        SAVE_FP_HIGH $t8

        csrrd $t0, 0x2
        andi $t0, $t0, 0x4
        beqz $t0, 1f
        addi.d $t8, $a0, {fp_lasx_hi0_offset}
        SAVE_FP_LASX_HI0 $t8
        addi.d $t8, $a0, {fp_lasx_hi1_offset}
        SAVE_FP_LASX_HI1 $t8
1:
        addi.d $t8, $a0, {fcc_offset}
        SAVE_FCC $t8
        addi.d $t8, $a0, {fcsr_offset}
        SAVE_FCSR $t8
        ret",
        fp_high_offset = const offset_of!(FpuState, fp_high),
        fp_lasx_hi0_offset = const offset_of!(FpuState, fp_lasx_hi0),
        fp_lasx_hi1_offset = const offset_of!(FpuState, fp_lasx_hi1),
        fcc_offset = const offset_of!(FpuState, fcc),
        fcsr_offset = const offset_of!(FpuState, fcsr),
    )
}

#[cfg(feature = "fp-simd")]
#[unsafe(naked)]
unsafe extern "C" fn restore_fp_registers(fpu: &FpuState) {
    naked_asm!(
        include_fp_asm_macros!(),
        "
        RESTORE_FP $a0
        addi.d $t8, $a0, {fp_high_offset}
        RESTORE_FP_HIGH $t8

        csrrd $t0, 0x2
        andi $t0, $t0, 0x4
        beqz $t0, 1f
        addi.d $t8, $a0, {fp_lasx_hi0_offset}
        RESTORE_FP_LASX_HI0 $t8
        addi.d $t8, $a0, {fp_lasx_hi1_offset}
        RESTORE_FP_LASX_HI1 $t8
1:
        addi.d $t8, $a0, {fcc_offset}
        RESTORE_FCC $t8
        addi.d $t8, $a0, {fcsr_offset}
        RESTORE_FCSR $t8
        ret",
        fp_high_offset = const offset_of!(FpuState, fp_high),
        fp_lasx_hi0_offset = const offset_of!(FpuState, fp_lasx_hi0),
        fp_lasx_hi1_offset = const offset_of!(FpuState, fp_lasx_hi1),
        fcc_offset = const offset_of!(FpuState, fcc),
        fcsr_offset = const offset_of!(FpuState, fcsr),
    )
}

#[cfg(feature = "tls")]
#[unsafe(naked)]
unsafe extern "C" fn context_switch_raw(_current_task: &mut TaskContext, _next_task: &TaskContext) {
    naked_asm!(
        include_asm_macros!(),
        "
        // save old context (callee-saved registers)
        st.d    $ra, $a0, {ra_offset}
        st.d    $sp, $a0, {sp_offset}
        st.d    $s0, $a0, {s0_offset}
        st.d    $s1, $a0, {s1_offset}
        st.d    $s2, $a0, {s2_offset}
        st.d    $s3, $a0, {s3_offset}
        st.d    $s4, $a0, {s4_offset}
        st.d    $s5, $a0, {s5_offset}
        st.d    $s6, $a0, {s6_offset}
        st.d    $s7, $a0, {s7_offset}
        st.d    $s8, $a0, {s8_offset}
        st.d    $fp, $a0, {frame_pointer_offset}
        // Keep task TLS inside the final, IRQ-disabled context-switch
        // boundary. In particular, never add the CPU-owned $r21 here.
        st.d    $tp, $a0, {kernel_tls_offset}

        // restore new context
        ld.d    $fp, $a1, {frame_pointer_offset}
        ld.d    $s8, $a1, {s8_offset}
        ld.d    $s7, $a1, {s7_offset}
        ld.d    $s6, $a1, {s6_offset}
        ld.d    $s5, $a1, {s5_offset}
        ld.d    $s4, $a1, {s4_offset}
        ld.d    $s3, $a1, {s3_offset}
        ld.d    $s2, $a1, {s2_offset}
        ld.d    $s1, $a1, {s1_offset}
        ld.d    $s0, $a1, {s0_offset}
        ld.d    $sp, $a1, {sp_offset}
        ld.d    $ra, $a1, {ra_offset}
        ld.d    $tp, $a1, {kernel_tls_offset}

        ret",
        ra_offset = const offset_of!(TaskContext, ra),
        sp_offset = const offset_of!(TaskContext, sp),
        s0_offset = const offset_of!(TaskContext, s),
        s1_offset = const offset_of!(TaskContext, s) + size_of::<[usize; 1]>(),
        s2_offset = const offset_of!(TaskContext, s) + size_of::<[usize; 2]>(),
        s3_offset = const offset_of!(TaskContext, s) + size_of::<[usize; 3]>(),
        s4_offset = const offset_of!(TaskContext, s) + size_of::<[usize; 4]>(),
        s5_offset = const offset_of!(TaskContext, s) + size_of::<[usize; 5]>(),
        s6_offset = const offset_of!(TaskContext, s) + size_of::<[usize; 6]>(),
        s7_offset = const offset_of!(TaskContext, s) + size_of::<[usize; 7]>(),
        s8_offset = const offset_of!(TaskContext, s) + size_of::<[usize; 8]>(),
        frame_pointer_offset = const offset_of!(TaskContext, s) + size_of::<[usize; 9]>(),
        kernel_tls_offset = const offset_of!(TaskContext, task_local)
            + offset_of!(TaskLocalState, kernel_tls),
    )
}

#[cfg(not(feature = "tls"))]
#[unsafe(naked)]
unsafe extern "C" fn context_switch_raw(_current_task: &mut TaskContext, _next_task: &TaskContext) {
    naked_asm!(
        include_asm_macros!(),
        "
        // Save old callee state. The CPU-owned r21/KS3 anchor and the
        // LinuxCurrent task-owned tp value are not generic saved registers.
        st.d    $ra, $a0, {ra_offset}
        st.d    $sp, $a0, {sp_offset}
        st.d    $s0, $a0, {s0_offset}
        st.d    $s1, $a0, {s1_offset}
        st.d    $s2, $a0, {s2_offset}
        st.d    $s3, $a0, {s3_offset}
        st.d    $s4, $a0, {s4_offset}
        st.d    $s5, $a0, {s5_offset}
        st.d    $s6, $a0, {s6_offset}
        st.d    $s7, $a0, {s7_offset}
        st.d    $s8, $a0, {s8_offset}
        st.d    $fp, $a0, {frame_pointer_offset}

        // Restore next state and make tp current immediately before the direct
        // return. r21 and KS3 continue to identify the physical CPU.
        ld.d    $fp, $a1, {frame_pointer_offset}
        ld.d    $s8, $a1, {s8_offset}
        ld.d    $s7, $a1, {s7_offset}
        ld.d    $s6, $a1, {s6_offset}
        ld.d    $s5, $a1, {s5_offset}
        ld.d    $s4, $a1, {s4_offset}
        ld.d    $s3, $a1, {s3_offset}
        ld.d    $s2, $a1, {s2_offset}
        ld.d    $s1, $a1, {s1_offset}
        ld.d    $s0, $a1, {s0_offset}
        ld.d    $sp, $a1, {sp_offset}
        ld.d    $ra, $a1, {ra_offset}
        ld.d    $tp, $a1, {current_header_offset}
        ret",
        ra_offset = const offset_of!(TaskContext, ra),
        sp_offset = const offset_of!(TaskContext, sp),
        s0_offset = const offset_of!(TaskContext, s),
        s1_offset = const offset_of!(TaskContext, s) + size_of::<[usize; 1]>(),
        s2_offset = const offset_of!(TaskContext, s) + size_of::<[usize; 2]>(),
        s3_offset = const offset_of!(TaskContext, s) + size_of::<[usize; 3]>(),
        s4_offset = const offset_of!(TaskContext, s) + size_of::<[usize; 4]>(),
        s5_offset = const offset_of!(TaskContext, s) + size_of::<[usize; 5]>(),
        s6_offset = const offset_of!(TaskContext, s) + size_of::<[usize; 6]>(),
        s7_offset = const offset_of!(TaskContext, s) + size_of::<[usize; 7]>(),
        s8_offset = const offset_of!(TaskContext, s) + size_of::<[usize; 8]>(),
        frame_pointer_offset = const offset_of!(TaskContext, s) + size_of::<[usize; 9]>(),
        current_header_offset = const offset_of!(TaskContext, task_local)
            + offset_of!(TaskLocalState, current_header),
    )
}
