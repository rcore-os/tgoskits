use axtest::prelude::*;

#[axtest]
fn axhal_irq_entry_keeps_irqs_disabled_until_preemption_is_reenabled() {
    let original_irq_state = ax_kernel_guard::IrqSave::new();
    crate::asm::enable_irqs();
    let observation = crate::irq::observe_irq_entry_state_for_test();
    drop(original_irq_state);

    ax_assert!(irq_entry_stages_hold(&observation));
    ax_assert!(observation.return_irqs_enabled);
}

#[axtest]
fn axhal_irq_entry_preserves_disabled_caller_state() {
    let caller_irq_guard = ax_kernel_guard::IrqSave::new();
    let observation = crate::irq::observe_irq_entry_state_for_test();
    drop(caller_irq_guard);

    ax_assert!(irq_entry_stages_hold(&observation));
    ax_assert!(!observation.return_irqs_enabled);
}

fn irq_entry_stages_hold(observation: &crate::irq::IrqEntryStateObservation) -> bool {
    !observation.dispatch_irqs_enabled && !observation.after_preempt_release_irqs_enabled
}
