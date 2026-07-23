use axtest::prelude::*;

#[cfg(target_arch = "x86_64")]
#[axtest]
fn somehal_x86_64_constants_hold() {
    ax_assert!(crate::arch::x86_64::somehal_x86_64_constants_hold_for_test());
}
