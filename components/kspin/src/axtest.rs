use axtest::prelude::*;

#[axtest]
fn kspin_rwlock_constants_hold() {
    ax_assert!(crate::rwlock::rwlock_constants_hold_for_test());
}
