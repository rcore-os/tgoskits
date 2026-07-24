use axtest::prelude::*;

#[axtest]
fn axfsng_block_irq_outcome_and_ready_hold() {
    #[cfg(feature = "axtest")]
    ax_assert!(crate::os::block_irq_outcome_and_ready_hold_for_test());
}
