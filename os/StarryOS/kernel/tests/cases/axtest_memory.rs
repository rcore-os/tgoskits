use axtest::prelude::*;
use starry_kernel::axtest_exports;

#[axtest]
fn nofault_user_read_recovers_unmapped_address() {
    ax_assert!(axtest_exports::nofault_user_read_recovers_unmapped_address());
}

#[axtest]
fn page_fault_completion_updates_only_success() {
    ax_assert!(axtest_exports::page_fault_completion_updates_only_success());
}

#[axtest]
fn cow_clone_failure_rollback_rules_hold() {
    ax_assert!(axtest_exports::cow_clone_failure_rollback_rules_hold());
}
