use axtest::prelude::*;

#[axtest]
fn axtask_api_constants_hold() {
    assert!(crate::api::axtask_api_constants_hold_for_test());
}
