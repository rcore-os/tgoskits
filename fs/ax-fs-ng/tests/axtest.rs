#![no_std]
#![no_main]

extern crate alloc;

use ax_fs_ng as _;
use ax_std as _;
use axtest::prelude::*;

#[axtest]
fn axfsng_block_irq_outcome_and_ready_hold() {
    #[cfg(feature = "axtest")]
    ax_assert!(ax_fs_ng::axtest_support::block_irq_outcome_and_ready_hold_for_test());
}

#[axtest::tests]
mod tests {}
