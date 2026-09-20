#![no_std]
#![no_main]
extern crate ax_std as std;

#[unsafe(no_mangle)]
fn main() {
    let irq = ax_cpu::interrupt::irqs_enabled();
    ax_cpu::interrupt::disable_irqs();
    let original_control: u64;
    let original_status: u64;
    let restored_control: u64;
    let restored_status: u64;
    let mut state = ax_cpu::registers::FpState::default();
    // SAFETY: the one-CPU test owns the FP register window with IRQs disabled.
    // It restores the original FP controls before calling the test reporter.
    unsafe {
        core::arch::asm!(".arch armv8", "mrs {}, fpcr", out(reg) original_control, options(nostack));
        core::arch::asm!(".arch armv8", "mrs {}, fpsr", out(reg) original_status, options(nostack));
        core::arch::asm!(".arch armv8", "msr fpcr, {}", "msr fpsr, {}", "isb",
            in(reg) (1u64 << 22), in(reg) 1u64, options(nostack));
    }
    state.save();
    let saved_control = state.fpcr;
    let saved_status = state.fpsr;
    state.fpcr = 2 << 22;
    state.fpsr = 2;
    state.restore();
    unsafe {
        core::arch::asm!(".arch armv8", "mrs {}, fpcr", out(reg) restored_control, options(nostack));
        core::arch::asm!(".arch armv8", "mrs {}, fpsr", out(reg) restored_status, options(nostack));
        core::arch::asm!(".arch armv8", "msr fpcr, {}", "msr fpsr, {}", "isb",
            in(reg) original_control, in(reg) original_status, options(nostack));
    }
    assert_eq!(saved_control, 1 << 22);
    assert_eq!(saved_status, 1, "saved FPSR must occupy its declared field");
    assert_eq!(restored_control, 2 << 22);
    assert_eq!(
        restored_status, 2,
        "restore must consume the declared FPSR field"
    );
    if irq {
        ax_cpu::interrupt::enable_irqs();
    }
    std::println!("CPU_FP_CONTEXT_OK");
}
