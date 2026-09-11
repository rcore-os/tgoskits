//! Local interrupt operations.

pub use riscv::{CoreInterruptNumber, InterruptNumber, interrupt::Interrupt};

pub use super::asm::{
    disable_irqs, enable_irqs, halt, irqs_enabled, wait_for_irqs, wait_for_irqs_disabled,
};

/// Enables a CPU-local supervisor interrupt source.
///
/// # Safety
/// The caller must have installed the source's trap handler and established
/// that delivering this source cannot violate a critical section.
pub unsafe fn enable_source(source: Interrupt) {
    // SAFETY: the caller owns the source and its interrupt-delivery contract.
    unsafe { riscv::interrupt::enable_interrupt(source) };
}

/// Disables a CPU-local supervisor source without changing global IRQ state.
pub fn disable_source(source: Interrupt) {
    riscv::interrupt::disable_interrupt(source);
}
