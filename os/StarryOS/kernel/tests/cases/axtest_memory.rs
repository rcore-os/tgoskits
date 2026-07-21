use axtest::prelude::*;
use starry_kernel::axtest_exports;

#[axtest::def_test]
fn process_mem_stats_formats_linux_fields() {
    ax_assert!(axtest_exports::process_mem_stats_formats_linux_fields());
}

#[axtest::def_test]
fn memory_accounting_tracks_cow_charge_transitions() {
    ax_assert!(axtest_exports::memory_accounting_tracks_cow_charge_transitions());
}

#[axtest::def_test]
fn memory_accounting_rejects_duplicate_and_conflicting_charges() {
    ax_assert!(axtest_exports::memory_accounting_rejects_duplicate_and_conflicting_charges());
}

#[axtest::def_test]
fn process_vm_stat_watermarks_hold() {
    ax_assert!(axtest_exports::process_vm_stat_watermarks_hold());
}

#[axtest::def_test]
fn user_pointer_metadata_rules_hold() {
    ax_assert!(axtest_exports::user_pointer_metadata_rules_hold());
}
