// ============================================================================
// 任务上下文管理（TODO: 多任务调度功能尚未实现）
// ============================================================================

// 异常处理临时寄存器保存
// CSR_KS0 (0x30), CSR_KS1 (0x31) 在 trap.rs 的汇编代码中使用
// CSR_KS2 (0x32) 当前未使用，保留备用
#[allow(dead_code)]
const EXCEPTION_KS0: usize = 0x30;
#[allow(dead_code)]
const EXCEPTION_KS1: usize = 0x31;
#[allow(dead_code)]
const EXCEPTION_KS2: usize = 0x32;

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

/// Floating-point registers of LoongArch64
/// 浮点寄存器状态（TODO: FPU 保存/恢复功能）
#[allow(dead_code)]
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct FpuState {
    /// Floating-point registers (f0-f31)
    pub fp: [u64; 32],
    /// Floating-point Condition Code register
    pub fcc: [u8; 8],
    /// Floating-point Control and Status register
    pub fcsr: u32,
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
    /// Gets the 0th syscall argument.
    /// 获取系统调用参数（TODO: 系统调用功能）
    #[allow(dead_code)]
    pub const fn arg0(&self) -> usize {
        self.regs.a0 as _
    }

    /// Gets the 1st syscall argument.
    #[allow(dead_code)]
    pub const fn arg1(&self) -> usize {
        self.regs.a1 as _
    }

    /// Gets the 2nd syscall argument.
    #[allow(dead_code)]
    pub const fn arg2(&self) -> usize {
        self.regs.a2 as _
    }

    /// Gets the 3rd syscall argument.
    #[allow(dead_code)]
    pub const fn arg3(&self) -> usize {
        self.regs.a3 as _
    }

    /// Gets the 4th syscall argument.
    #[allow(dead_code)]
    pub const fn arg4(&self) -> usize {
        self.regs.a4 as _
    }

    /// Gets the 5th syscall argument.
    #[allow(dead_code)]
    pub const fn arg5(&self) -> usize {
        self.regs.a5 as _
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
///
/// 任务上下文（TODO: 多任务调度功能）
#[allow(dead_code)]
#[allow(missing_docs)]
#[repr(C)]
#[derive(Debug, Default)]
pub struct TaskContext {
    /// Return Address
    pub ra: usize,
    /// Stack Pointer
    pub sp: usize,
    /// loongArch need to save 10 static registers from $r22 to $r31
    pub s: [usize; 10],
    /// Thread Pointer
    pub tp: usize,
}

impl TaskContext {
    /// Creates a new default context for a new task.
    #[allow(dead_code)]
    pub fn new() -> Self {
        Self::default()
    }

    // /// Initializes the context for a new task, with the given entry point and
    // /// kernel stack.
    // pub fn init(&mut self, entry: usize, kstack_top: VirtAddr, tls_area: VirtAddr) {
    //     self.sp = kstack_top.as_usize();
    //     self.ra = entry;
    //     self.tp = tls_area.as_usize();
    // }

    /// Switches to another task.
    ///
    /// It first saves the current task's context from CPU to this place, and then
    /// restores the next task's context from `next_ctx` to CPU.
    #[allow(dead_code)]
    pub fn switch_to(&mut self, _next_ctx: &Self) {
        // TODO: 实现任务上下文切换
        // unsafe { context_switch(self, next_ctx) }
    }
}

// #[unsafe(naked)]
// unsafe extern "C" fn context_switch(_current_task: &mut TaskContext, _next_task: &TaskContext) {
//     naked_asm!(
//         include_asm_macros!(),
//         "
//         // save old context (callee-saved registers)
//         STD     $ra, $a0, 0
//         STD     $sp, $a0, 1
//         STD     $s0, $a0, 2
//         STD     $s1, $a0, 3
//         STD     $s2, $a0, 4
//         STD     $s3, $a0, 5
//         STD     $s4, $a0, 6
//         STD     $s5, $a0, 7
//         STD     $s6, $a0, 8
//         STD     $s7, $a0, 9
//         STD     $s8, $a0, 10
//         STD     $fp, $a0, 11

//         // restore new context
//         LDD     $fp, $a1, 11
//         LDD     $s8, $a1, 10
//         LDD     $s7, $a1, 9
//         LDD     $s6, $a1, 8
//         LDD     $s5, $a1, 7
//         LDD     $s4, $a1, 6
//         LDD     $s3, $a1, 5
//         LDD     $s2, $a1, 4
//         LDD     $s1, $a1, 3
//         LDD     $s0, $a1, 2
//         LDD     $sp, $a1, 1
//         LDD     $ra, $a1, 0

//         ret",
//     )
// }
