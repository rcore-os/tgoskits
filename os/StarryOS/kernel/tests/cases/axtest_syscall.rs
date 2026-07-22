use axtest::prelude::*;
use starry_kernel::axtest_exports;

#[axtest]
fn bpf_unknown_command_is_invalid() {
    ax_assert!(axtest_exports::bpf_unknown_command_is_invalid());
}

#[axtest]
fn credential_capability_rules_hold() {
    ax_assert!(axtest_exports::credential_capability_rules_hold());
}

#[axtest]
fn resource_limit_defaults_hold() {
    ax_assert!(axtest_exports::resource_limit_defaults_hold());
}

#[axtest]
fn seccomp_filter_rules_hold() {
    ax_assert!(axtest_exports::seccomp_filter_rules_hold());
}

#[axtest]
fn seccomp_filter_construction_rules_hold() {
    ax_assert!(axtest_exports::seccomp_filter_construction_rules_hold());
}

#[axtest]
fn rseq_validation_rejects_invalid_arguments() {
    ax_assert!(axtest_exports::rseq_validation_rejects_invalid_arguments());
}

#[axtest]
fn membarrier_validation_rules_hold() {
    ax_assert!(axtest_exports::membarrier_validation_rules_hold());
}

#[axtest]
fn mempolicy_validation_rules_hold() {
    ax_assert!(axtest_exports::mempolicy_validation_rules_hold());
}

#[axtest]
fn task_clone_validation_rules_hold() {
    ax_assert!(axtest_exports::task_clone_validation_rules_hold());
}

#[axtest]
fn capability_data_conversion_rules_hold() {
    ax_assert!(axtest_exports::capability_data_conversion_rules_hold());
}
