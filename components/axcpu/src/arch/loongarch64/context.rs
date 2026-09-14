#[cfg(feature = "context")]
use core::arch::naked_asm;
#[cfg(feature = "context")]
use core::mem::{align_of, offset_of, size_of};

#[cfg(feature = "context")]
use ax_memory_addr::VirtAddr;

#[cfg(feature = "fp-simd")]
use super::fp::FpuState;
use super::registers::GeneralRegisters;
#[cfg(feature = "context")]
use crate::{KernelTlsBase, TaskLocalState, context::TaskAnchor};

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
    /// Copies IRQ sampling values from this saved machine image.
    pub fn interrupted_context(&self) -> crate::trap::InterruptedContext {
        use crate::trap::{InterruptedContext, InterruptedPrivilege};
        InterruptedContext {
            pc: self.ip(),
            sp: self.regs.sp,
            fp: self.regs.fp,
            privilege: match self.origin() {
                crate::trap::TrapOrigin::Kernel => InterruptedPrivilege::Kernel,
                crate::trap::TrapOrigin::User => InterruptedPrivilege::User,
            },
        }
    }

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

    /// Copies the machine registers needed by a runtime stack unwinder.
    pub fn backtrace_registers(&self) -> crate::trap::BacktraceRegisters {
        crate::trap::BacktraceRegisters::new(self.regs.fp as _, self.era as _, self.regs.ra as _)
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
#[cfg(feature = "context")]
pub struct TaskContext {
    /// Return Address
    pub ra: usize,
    /// Stack Pointer
    pub sp: usize,
    /// loongArch need to save 10 static registers from $r22 to $r31
    pub s: [usize; 10],
    /// Architecture-neutral current-header and kernel-TLS switch state.
    task_local: TaskLocalState,
    #[cfg(feature = "fp-simd")]
    /// Floating Point Unit states
    pub fpu: FpuState,
}

// The naked switch uses one machine-word load/store for each field. Keep the
// array packing and TLS representation assumptions checked by the compiler.
#[cfg(feature = "context")]
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

#[cfg(feature = "context")]
impl TaskContext {
    /// Creates a new default context for a new task.
    pub fn new() -> Self {
        Self::default()
    }

    /// Initializes a task context with its entry point, kernel stack, and
    /// task-owned kernel TLS base.
    pub fn init(&mut self, entry: usize, kstack_top: VirtAddr, kernel_tls: KernelTlsBase) {
        self.sp = kstack_top.as_usize();
        self.ra = entry;
        self.task_local.set_kernel_tls(kernel_tls);
    }

    /// Sets the pinned task-owned runtime task anchor.
    pub fn set_task_anchor(&mut self, header: TaskAnchor) {
        self.task_local.set_task_anchor(header);
    }

    /// Returns the configured task-owned runtime task anchor.
    pub const fn task_anchor(&self) -> Option<TaskAnchor> {
        self.task_local.task_anchor()
    }

    /// Saves the running parent's hardware FP image into an unpublished child.
    ///
    /// The runtime must pin the parent CPU while calling this method. Kernel
    /// code uses the soft-float ABI; eager task switching keeps the parent's
    /// user register image live across kernel entry and preemption.
    #[cfg(all(feature = "fp-simd", feature = "uspace"))]
    pub fn clone_user_fp_state_into(&self, child: &mut Self) {
        assert!(
            self.task_anchor().is_some(),
            "FP clone parent must be bound"
        );
        assert!(
            child.task_anchor().is_none(),
            "FP clone child must be unpublished"
        );
        child.fpu.save();
    }

    /// Completes FPU work before current-context publication.
    pub fn prepare_switch_to(&mut self, _next_ctx: &Self) {
        #[cfg(feature = "fp-simd")]
        {
            self.fpu.save();
            _next_ctx.fpu.restore();
        }
    }

    /// Performs only the final GPR/current/TLS transfer.
    ///
    /// # Safety
    ///
    /// Scheduling must be serialized, FPU state prepared, and the next current
    /// anchor published. Both contexts and their anchors must remain pinned and
    /// alive. IRQs must remain disabled from publication through this call,
    /// with no intervening fallible or ownership-sensitive work.
    #[inline(always)]
    pub unsafe fn switch_to(&mut self, next_ctx: &Self) {
        unsafe { context_switch_raw(self, next_ctx) }
    }
}

#[cfg(kernel_tls)]
#[unsafe(naked)]
#[cfg(feature = "context")]
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

#[cfg(not(kernel_tls))]
#[unsafe(naked)]
#[cfg(feature = "context")]
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
        ld.d    $tp, $a1, {context_header_offset}
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
        context_header_offset = const offset_of!(TaskContext, task_local)
            + offset_of!(TaskLocalState, context_header),
    )
}
