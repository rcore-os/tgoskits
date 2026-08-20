use axtest::prelude::*;
use starry_kernel::axtest_exports;

#[axtest]
fn reaping_identity_is_not_publicly_resolvable() {
    ax_assert!(axtest_exports::reaping_identity_is_not_publicly_resolvable());
}

#[axtest]
fn duplicate_live_session_identity_is_rejected() {
    ax_assert!(axtest_exports::duplicate_live_session_identity_is_rejected());
}

#[axtest]
fn prepared_descriptor_stays_hidden_until_install() {
    ax_assert!(axtest_exports::prepared_descriptor_stays_hidden_until_install());
}

#[axtest]
fn futex_nofault_failure_is_transactional() {
    ax_assert!(axtest_exports::futex_nofault_failure_is_transactional());
}

#[axtest]
fn pid_identity_state_machine_rules_hold() {
    ax_assert!(axtest_exports::pid_identity_state_machine_rules_hold());
}
