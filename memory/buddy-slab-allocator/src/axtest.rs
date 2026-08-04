use axtest::prelude::*;

#[axtest]
fn buddy_slab_page_constants_and_header_helpers_hold() {
    ax_assert!(crate::slab::slab_page_constants_and_header_helpers_hold_for_test());
}
