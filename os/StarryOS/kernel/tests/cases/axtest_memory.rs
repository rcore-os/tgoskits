use axtest::prelude::*;
use starry_kernel::axtest_exports;

#[axtest]
fn process_mem_stats_formats_linux_fields() {
    ax_assert!(axtest_exports::process_mem_stats_formats_linux_fields());
}

#[axtest]
fn memory_accounting_tracks_cow_charge_transitions() {
    ax_assert!(axtest_exports::memory_accounting_tracks_cow_charge_transitions());
}

#[axtest]
fn memory_accounting_rejects_duplicate_and_conflicting_charges() {
    ax_assert!(axtest_exports::memory_accounting_rejects_duplicate_and_conflicting_charges());
}

#[axtest]
fn process_vm_stat_watermarks_hold() {
    ax_assert!(axtest_exports::process_vm_stat_watermarks_hold());
}

#[axtest]
fn user_pointer_metadata_rules_hold() {
    ax_assert!(axtest_exports::user_pointer_metadata_rules_hold());
}

#[axtest]
fn cow_file_max_read_len_boundary_rules_hold() {
    ax_assert!(axtest_exports::cow_file_max_read_len_boundary_rules_hold());
}

#[axtest]
fn stats_classify_and_accumulate_rules_hold() {
    ax_assert!(axtest_exports::stats_classify_and_accumulate_rules_hold());
}
