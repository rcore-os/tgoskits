#[cfg(feature = "context")]
use core::arch::naked_asm;
use core::fmt;
#[cfg(feature = "context")]
use core::mem::{align_of, offset_of, size_of};

#[cfg(feature = "context")]
use ax_memory_addr::VirtAddr;

#[cfg(feature = "fp-simd")]
use super::fp::FpState;
#[cfg(feature = "context")]
use crate::{KernelTlsBase, TaskLocalState, context::TaskAnchor};

/// Saved registers when a trap (exception) occurs.
#[repr(C)]
#[derive(Default, Clone, Copy)]
pub struct TrapFrame {
    /// General-purpose registers (X0..X30).
    pub x: super::registers::GeneralRegisters,
    /// Exception Link Register (ELR_EL1).
    pub elr: u64,
    /// Saved Process Status Register (SPSR_EL1).
    pub spsr: u64,

    /// Stack pointer at the time of the exception.
    /// Populated by SAVE_REGS as `sp_before_sub = sp_after_sub + trapframe_size`.
    ///
    /// Note: This field is read-only (saved by SAVE_REGS for inspection only).
    /// The actual SP is restored by RESTORE_REGS via `add sp, sp, #trapframe_size`,
    /// not from this field. Modifying this value will NOT affect the actual SP
    /// after exception return.
    pub sp: u64,
}

impl fmt::Debug for TrapFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "TrapFrame: {{")?;
        for (i, &reg) in self.x.iter().enumerate() {
            writeln!(f, "    x{i}: {reg:#x},")?;
        }
        writeln!(f, "    elr: {:#x},", self.elr)?;
        writeln!(f, "    spsr: {:#x},", self.spsr)?;
        writeln!(f, "    sp: {:#x},", self.sp)?;
        write!(f, "}}")?;
        Ok(())
    }
}

impl TrapFrame {
    /// Copies IRQ sampling values from this saved machine image.
    pub fn interrupted_context(&self) -> crate::trap::InterruptedContext {
        use crate::trap::{InterruptedContext, InterruptedPrivilege};
        InterruptedContext {
            pc: self.ip(),
            sp: self.sp as usize,
            fp: self.x[29] as usize,
            privilege: match self.origin() {
                crate::trap::TrapOrigin::Kernel => InterruptedPrivilege::Kernel,
                crate::trap::TrapOrigin::User => InterruptedPrivilege::User,
            },
        }
    }

    /// Returns the privilege domain represented by this register image.
    pub const fn origin(&self) -> crate::TrapOrigin {
        if self.spsr & 0b1_1111 == 0 {
            crate::TrapOrigin::User
        } else {
            crate::TrapOrigin::Kernel
        }
    }

    /// Gets the 0th syscall argument.
    pub const fn arg0(&self) -> usize {
        self.x[0] as _
    }

    /// Sets the 0th syscall argument.
    pub const fn set_arg0(&mut self, a0: usize) {
        self.x[0] = a0 as _;
    }

    /// Gets the 1st syscall argument.
    pub const fn arg1(&self) -> usize {
        self.x[1] as _
    }

    /// Sets the 1st syscall argument.
    pub const fn set_arg1(&mut self, a1: usize) {
        self.x[1] = a1 as _;
    }

    /// Gets the 2nd syscall argument.
    pub const fn arg2(&self) -> usize {
        self.x[2] as _
    }

    /// Sets the 2nd syscall argument.
    pub const fn set_arg2(&mut self, a2: usize) {
        self.x[2] = a2 as _;
    }

    /// Gets the 3rd syscall argument.
    pub const fn arg3(&self) -> usize {
        self.x[3] as _
    }

    /// Sets the 3rd syscall argument.
    pub const fn set_arg3(&mut self, a3: usize) {
        self.x[3] = a3 as _;
    }

    /// Gets the 4th syscall argument.
    pub const fn arg4(&self) -> usize {
        self.x[4] as _
    }

    /// Sets the 4th syscall argument.
    pub const fn set_arg4(&mut self, a4: usize) {
        self.x[4] = a4 as _;
    }

    /// Gets the 5th syscall argument.
    pub const fn arg5(&self) -> usize {
        self.x[5] as _
    }

    /// Sets the 5th syscall argument.
    pub const fn set_arg5(&mut self, a5: usize) {
        self.x[5] = a5 as _;
    }

    /// Gets the instruction pointer.
    pub const fn ip(&self) -> usize {
        self.elr as _
    }

    /// Sets the instruction pointer.
    pub const fn set_ip(&mut self, pc: usize) {
        self.elr = pc as _;
    }

    /// Get the syscall number.
    pub const fn sysno(&self) -> usize {
        self.x[8] as usize
    }

    /// Sets the syscall number.
    pub const fn set_sysno(&mut self, sysno: usize) {
        self.x[8] = sysno as _;
    }

    /// Gets the return value register.
    pub const fn retval(&self) -> usize {
        self.x[0] as _
    }

    /// Sets the return value register.
    pub const fn set_retval(&mut self, r0: usize) {
        self.x[0] = r0 as _;
    }

    /// Sets the return address.
    pub const fn set_ra(&mut self, lr: usize) {
        self.x[30] = lr as _;
    }

    /// Copies the machine registers needed by a runtime stack unwinder.
    pub fn backtrace_registers(&self) -> crate::trap::BacktraceRegisters {
        crate::trap::BacktraceRegisters::new(self.x[29] as _, self.elr as _, self.x[30] as _)
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
    sp: u64,
    r19: u64,
    r20: u64,
    r21: u64,
    r22: u64,
    r23: u64,
    r24: u64,
    r25: u64,
    r26: u64,
    r27: u64,
    r28: u64,
    r29: u64,
    lr: u64, // r30
    /// Architecture-neutral current-header and kernel-TLS switch state.
    task_local: TaskLocalState,
    #[cfg(feature = "fp-simd")]
    fp_state: FpState,
}

// `stp`/`ldp` address only the first member of each pair. Prove the paired
// fields are adjacent and that the task TLS newtype has the register width.
#[cfg(feature = "context")]
const _: () = {
    assert!(size_of::<KernelTlsBase>() == size_of::<usize>());
    assert!(align_of::<KernelTlsBase>() == align_of::<usize>());
    assert!(offset_of!(TaskContext, sp) == 0);
    assert!(offset_of!(TaskContext, r20) == offset_of!(TaskContext, r19) + size_of::<u64>());
    assert!(offset_of!(TaskContext, r22) == offset_of!(TaskContext, r21) + size_of::<u64>());
    assert!(offset_of!(TaskContext, r24) == offset_of!(TaskContext, r23) + size_of::<u64>());
    assert!(offset_of!(TaskContext, r26) == offset_of!(TaskContext, r25) + size_of::<u64>());
    assert!(offset_of!(TaskContext, r28) == offset_of!(TaskContext, r27) + size_of::<u64>());
    assert!(offset_of!(TaskContext, lr) == offset_of!(TaskContext, r29) + size_of::<u64>());
    assert!(offset_of!(TaskContext, task_local) == offset_of!(TaskContext, lr) + size_of::<u64>());
};

#[cfg(feature = "context")]
impl TaskContext {
    /// Creates a dummy context for a new task.
    ///
    /// Note the context is not initialized, it will be filled by
    /// [`switch_to`](Self::switch_to) (for initial tasks) and [`init`]
    /// (for regular tasks) methods.
    ///
    /// [`init`]: TaskContext::init
    pub fn new() -> Self {
        Self::default()
    }

    /// Initializes the context for a new task, with the given entry point and
    /// kernel stack.
    pub fn init(&mut self, entry: usize, kstack_top: VirtAddr, kernel_tls: KernelTlsBase) {
        self.sp = kstack_top.as_usize() as u64;
        self.lr = entry as u64;
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
        child.fp_state.save();
    }

    /// Completes FP/SIMD work before current-context publication.
    pub fn prepare_switch_to(&mut self, _next_ctx: &Self) {
        #[cfg(feature = "fp-simd")]
        {
            self.fp_state.save();
            _next_ctx.fp_state.restore();
        }
    }

    /// Performs only the final GPR/current/TLS transfer.
    ///
    /// # Safety
    ///
    /// Scheduling must be serialized, FP state prepared, and the next current
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
        "
        // save old context (callee-saved registers)
        stp     x29, x30, [x0, {r29_offset}]
        stp     x27, x28, [x0, {r27_offset}]
        stp     x25, x26, [x0, {r25_offset}]
        stp     x23, x24, [x0, {r23_offset}]
        stp     x21, x22, [x0, {r21_offset}]
        stp     x19, x20, [x0, {r19_offset}]
        mov     x19, sp
        str     x19, [x0, {sp_offset}]
        mrs     x9, tpidr_el0
        str     x9, [x0, {kernel_tls_offset}]

        // restore new context
        ldr     x9, [x1, {kernel_tls_offset}]
        msr     tpidr_el0, x9
        ldr     x19, [x1, {sp_offset}]
        mov     sp, x19
        ldp     x19, x20, [x1, {r19_offset}]
        ldp     x21, x22, [x1, {r21_offset}]
        ldp     x23, x24, [x1, {r23_offset}]
        ldp     x25, x26, [x1, {r25_offset}]
        ldp     x27, x28, [x1, {r27_offset}]
        ldp     x29, x30, [x1, {r29_offset}]
        ldr     x9, [x1, {context_header_offset}]
        msr     sp_el0, x9

        ret",
        sp_offset = const offset_of!(TaskContext, sp),
        r19_offset = const offset_of!(TaskContext, r19),
        r21_offset = const offset_of!(TaskContext, r21),
        r23_offset = const offset_of!(TaskContext, r23),
        r25_offset = const offset_of!(TaskContext, r25),
        r27_offset = const offset_of!(TaskContext, r27),
        r29_offset = const offset_of!(TaskContext, r29),
        context_header_offset = const offset_of!(TaskContext, task_local)
            + offset_of!(TaskLocalState, context_header),
        kernel_tls_offset = const offset_of!(TaskContext, task_local)
            + offset_of!(TaskLocalState, kernel_tls),
    )
}

#[cfg(not(kernel_tls))]
#[unsafe(naked)]
#[cfg(feature = "context")]
unsafe extern "C" fn context_switch_raw(_current_task: &mut TaskContext, _next_task: &TaskContext) {
    naked_asm!(
        "
        // save old context (callee-saved registers)
        stp     x29, x30, [x0, {r29_offset}]
        stp     x27, x28, [x0, {r27_offset}]
        stp     x25, x26, [x0, {r25_offset}]
        stp     x23, x24, [x0, {r23_offset}]
        stp     x21, x22, [x0, {r21_offset}]
        stp     x19, x20, [x0, {r19_offset}]
        mov     x19, sp
        str     x19, [x0, {sp_offset}]

        // LinuxCurrent keeps task identity in SP_EL0. TPIDR_EL0 remains
        // userspace-owned and is never part of a kernel task switch.
        ldr     x19, [x1, {sp_offset}]
        mov     sp, x19
        ldp     x19, x20, [x1, {r19_offset}]
        ldp     x21, x22, [x1, {r21_offset}]
        ldp     x23, x24, [x1, {r23_offset}]
        ldp     x25, x26, [x1, {r25_offset}]
        ldp     x27, x28, [x1, {r27_offset}]
        ldp     x29, x30, [x1, {r29_offset}]
        ldr     x9, [x1, {context_header_offset}]
        msr     sp_el0, x9
        ret",
        sp_offset = const offset_of!(TaskContext, sp),
        r19_offset = const offset_of!(TaskContext, r19),
        r21_offset = const offset_of!(TaskContext, r21),
        r23_offset = const offset_of!(TaskContext, r23),
        r25_offset = const offset_of!(TaskContext, r25),
        r27_offset = const offset_of!(TaskContext, r27),
        r29_offset = const offset_of!(TaskContext, r29),
        context_header_offset = const offset_of!(TaskContext, task_local)
            + offset_of!(TaskLocalState, context_header),
    )
}
