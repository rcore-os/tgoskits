//! System-timer surface for architectures whose timer arming is driven by
//! someboot's `SystimerArch` capability (Arm generic timer, RISC-V SBI timer,
//! LoongArch TCG). On x86_64 the system timer lives inside the local APIC and
//! [`crate::arch::x86_64::timer`] replaces this module.

// The counter domain stays in someboot, which owns the hardware counter.
pub use someboot::timer::{
    CounterStability, duration_to_ticks, elapsed, freq, scheduler_clock_stability, since_boot,
    ticks, ticks_to_duration,
};
use someboot::{SystimerArch, arch::Arch};

/// Prepares the system timer in a masked, non-firing state.
pub fn enable() {
    Arch::systimer_enable();
}

/// Unmasks the timer interrupt.
pub fn irq_enable() {
    Arch::systimer_irq_enable();
}

/// Returns whether IRQ claim must stop the source before controller EOI.
pub fn requires_irq_quiesce() -> bool {
    Arch::systimer_requires_irq_quiesce()
}

/// Cancels the active one-shot and discards its comparator state.
pub fn cancel_oneshot() {
    Arch::systimer_cancel_oneshot();
}

/// Restores a cancelled one-shot at an absolute counter deadline.
pub fn resume_oneshot_at_ticks(deadline_ticks: u64) {
    Arch::systimer_resume_oneshot(deadline_ticks);
}

/// Arms a one-shot at an absolute counter deadline.
pub fn set_next_event_at_ticks(deadline_ticks: u64) {
    Arch::set_next_event_at_ticks(deadline_ticks);
}

/// Acknowledge and clear the timer interrupt.
pub fn ack() {
    Arch::systimer_ack();
}
