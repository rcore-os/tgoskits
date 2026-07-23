use axtest::prelude::*;

#[axtest]
fn someboot_mem_constants_and_cache_line_rules_hold() {
    ax_assert!(crate::mem::mem_constants_and_cache_line_rules_hold_for_test());
}
