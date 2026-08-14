use axtest::prelude::*;
use starry_kernel::axtest_exports;

#[axtest]
fn tun_rollback_destroys_created_device() {
    ax_assert!(axtest_exports::tun_rollback_destroys_created_device());
}

#[axtest]
fn tun_rollback_detaches_existing_device() {
    ax_assert!(axtest_exports::tun_rollback_detaches_existing_device());
}

#[axtest]
fn tun_rollback_on_concurrent_close() {
    ax_assert!(axtest_exports::tun_rollback_on_concurrent_close());
}

#[axtest]
fn tun_concurrent_claim_has_single_winner() {
    ax_assert!(axtest_exports::tun_concurrent_claim_has_single_winner());
}

#[axtest]
fn tun_close_dying_latch_blocks_attach() {
    ax_assert!(axtest_exports::tun_close_dying_latch_blocks_attach());
}
