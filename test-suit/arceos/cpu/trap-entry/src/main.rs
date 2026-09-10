#![no_std]
#![no_main]
extern crate ax_std as std;

use core::sync::atomic::{AtomicUsize, Ordering};

use ax_cpu::trap::{InterruptedContext, InterruptedPrivilege, KernelTrapFrame, TrapOrigin};

static SAVED_PC: AtomicUsize = AtomicUsize::new(0);
static SAVED_SP: AtomicUsize = AtomicUsize::new(0);
static SAVED_FP: AtomicUsize = AtomicUsize::new(0);
static OBSERVED: AtomicUsize = AtomicUsize::new(0);

fn record(context: InterruptedContext) {
    if context.privilege == InterruptedPrivilege::Kernel {
        SAVED_PC.store(context.pc, Ordering::Relaxed);
        SAVED_SP.store(context.sp, Ordering::Relaxed);
        SAVED_FP.store(context.fp, Ordering::Relaxed);
        OBSERVED.fetch_add(1, Ordering::Relaxed);
    }
}

fn breakpoint(frame: &mut KernelTrapFrame<'_>) -> bool {
    let mut saved = frame.snapshot();
    record(saved.interrupted_context());
    saved.regs.set_r10d(0x89ab_cdef);
    frame.apply_registers(&saved);
    true
}

fn irq(vector: usize, origin: TrapOrigin, context: Option<InterruptedContext>) -> bool {
    if vector == 0xf0 && origin == TrapOrigin::Kernel {
        if let Some(context) = context {
            record(context);
        }
        true
    } else {
        false
    }
}

#[unsafe(no_mangle)]
fn main() {
    // One CPU and masked hardware IRQs make replacing the two runtime hooks
    // exclusive. Software INT still exercises the real IDT and assembly entry.
    let irqs = ax_cpu::interrupt::irqs_enabled();
    ax_cpu::interrupt::disable_irqs();
    let previous_breakpoint = ax_cpu::trap::set_breakpoint_handler(breakpoint);
    let expected_sp: usize;
    let expected_fp: usize;
    let expected_pc: usize;
    let restored_r10: u64;
    // SAFETY: the installed handler resumes this exact kernel breakpoint.
    // Explicit outputs describe all modified registers; the trap restores RSP.
    unsafe {
        core::arch::asm!(
            "mov rax, rsp",
            "mov r8, rbp",
            "lea r11, [rip + 2f]",
            "mov r10, -1",
            "int3",
            "2:",
            out("rax") expected_sp,
            out("r8") expected_fp,
            out("r11") expected_pc,
            out("r10") restored_r10,
        );
    }
    ax_cpu::trap::set_breakpoint_handler(previous_breakpoint);
    let breakpoint_context = (
        SAVED_SP.load(Ordering::Relaxed),
        SAVED_FP.load(Ordering::Relaxed),
        SAVED_PC.load(Ordering::Relaxed),
    );
    let breakpoint_expected = (expected_sp, expected_fp, expected_pc);

    let previous_irq = ax_cpu::trap::set_irq_handler(irq);
    let irq_sp: usize;
    let irq_fp: usize;
    let irq_pc: usize;
    // SAFETY: this software vector uses the installed CPU trap hook, requires
    // no controller acknowledge, and returns to its own kernel continuation.
    unsafe {
        core::arch::asm!(
            "mov rax, rsp",
            "mov r8, rbp",
            "lea r11, [rip + 2f]",
            "int 0xf0",
            "2:",
            out("rax") irq_sp,
            out("r8") irq_fp,
            out("r11") irq_pc,
        );
    }
    ax_cpu::trap::set_irq_handler(previous_irq);
    let irq_context = (
        SAVED_SP.load(Ordering::Relaxed),
        SAVED_FP.load(Ordering::Relaxed),
        SAVED_PC.load(Ordering::Relaxed),
    );
    if irqs {
        ax_cpu::interrupt::enable_irqs();
    }
    assert_eq!(OBSERVED.load(Ordering::Relaxed), 2);
    assert_eq!(
        breakpoint_context, breakpoint_expected,
        "kernel breakpoint PC/SP/FP"
    );
    assert_eq!(irq_context, (irq_sp, irq_fp, irq_pc), "kernel IRQ PC/SP/FP");
    assert_eq!(
        restored_r10, 0x89ab_cdef,
        "trap return must restore the edited GPR image"
    );
    std::println!("CPU_TRAP_ENTRY_OK");
}
