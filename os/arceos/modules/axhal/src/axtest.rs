use axtest::prelude::*;

#[axtest]
fn axhal_irq_entry_disables_and_restores_local_irqs() {
    ax_assert!(crate::irq::irq_entry_state_rules_hold_for_test());
}
