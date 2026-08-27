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

/// Brings the system timer into its enabled state.
pub fn enable() {
    Arch::systimer_enable();
}

/// Unmasks the timer interrupt.
pub fn irq_enable() {
    Arch::systimer_irq_enable();
}

/// Arms a one-shot deadline `ticks` from now.
pub fn set_next_event_in_ticks(ticks: usize) {
    Arch::set_next_event_in_ticks(ticks);
}

/// Acknowledge and clear the timer interrupt.
pub fn ack() {
    Arch::systimer_ack();
}
