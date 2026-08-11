use axtest::prelude::*;

#[axtest]
fn spin_rwlock_constants_hold() {
    ax_assert!(crate::sync::spin::rwlock::rwlock_constants_hold_for_test());
}

#[axtest]
fn spin_rwlock_state_logic_hold() {
    ax_assert!(crate::sync::spin::rwlock::rwlock_state_logic_hold_for_test());
}

#[axtest]
fn spin_rwlock_constants_and_phantom_hold() {
    ax_assert!(crate::sync::spin::rwlock::rwlock_constants_and_phantom_hold_for_test());
}

#[axtest]
fn spin_rwlock_state_transitions_hold() {
    ax_assert!(crate::sync::spin::rwlock::rwlock_state_transitions_hold_for_test());
}

#[axtest]
fn spin_rwlock_guard_types_hold() {
    ax_assert!(crate::sync::spin::rwlock::rwlock_guard_types_hold_for_test());
}

#[axtest]
fn spin_rwlock_lockdep_and_feature_config_hold() {
    ax_assert!(crate::sync::spin::rwlock::rwlock_lockdep_and_feature_config_hold_for_test());
}

#[axtest]
fn spin_rwlock_reader_writer_state_combinations_hold() {
    ax_assert!(crate::sync::spin::rwlock::rwlock_reader_writer_state_combinations_hold_for_test());
}
