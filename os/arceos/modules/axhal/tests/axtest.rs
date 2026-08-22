#![no_std]
#![no_main]

extern crate alloc;

use ax_hal as _;
use ax_std as _;
use axtest::prelude::*;

#[axtest]
fn axhal_irq_entry_keeps_irqs_disabled_until_preemption_is_reenabled() {
    let original_irq_state = ax_sync::IrqSaveGuard::new();
    ax_hal::asm::enable_irqs();
    let observation = ax_hal::axtest_support::observe_irq_entry_state_for_test();
    drop(original_irq_state);

    ax_assert!(irq_entry_stages_hold(&observation));
    ax_assert!(observation.2);
}

#[axtest]
fn axhal_irq_entry_preserves_disabled_caller_state() {
    let caller_irq_guard = ax_sync::IrqSaveGuard::new();
    let observation = ax_hal::axtest_support::observe_irq_entry_state_for_test();
    drop(caller_irq_guard);

    ax_assert!(irq_entry_stages_hold(&observation));
    ax_assert!(!observation.2);
}

fn irq_entry_stages_hold(observation: &(bool, bool, bool)) -> bool {
    !observation.0 && !observation.1
}

#[axtest::tests]
mod tests {}
