use axtest::prelude::*;
use starry_kernel::axtest_exports;

#[axtest::def_test]
fn user_stack_layout_is_inside_user_space() {
    ax_assert!(axtest_exports::user_space_base() < axtest_exports::user_stack_top());
    ax_assert!(axtest_exports::user_stack_size() > 0);
    ax_assert!(
        axtest_exports::user_stack_top()
            <= axtest_exports::user_space_base() + axtest_exports::user_space_size()
    );
}

#[axtest::def_test]
fn signal_trampoline_is_page_aligned() {
    ax_assert_eq!(axtest_exports::signal_trampoline() & 0xfff, 0);
}

#[axtest::def_test]
fn timespec_rejects_invalid_nsec() {
    ax_assert!(axtest_exports::invalid_timespec_is_rejected());
}

#[axtest::def_test]
fn random_write_mixes_entropy() {
    ax_assert!(axtest_exports::random_write_mixes_entropy());
}

#[axtest::def_test]
fn time_value_conversion_rules_hold() {
    ax_assert!(axtest_exports::time_value_conversion_rules_hold());
}
