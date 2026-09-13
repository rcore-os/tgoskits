use super::*;
use crate::sched::{FairMode, Nice};

#[test]
fn ordinary_weighted_sum_uses_native_width_division() {
    assert!(!weighted_sum_needs_wide_division(-40_960, 40_960));
    assert_eq!(divide_weighted_sum(-40_960, 40_960), -1);
    assert!(weighted_sum_needs_wide_division(
        i128::from(i64::MAX) + 1,
        1
    ));
    assert_eq!(
        divide_weighted_sum(i128::from(i64::MAX) + 1, 2),
        (i128::from(i64::MAX) + 1) / 2
    );
}

#[test]
fn linux_run_to_parity_keeps_an_eligible_protected_current() {
    let mut current = FairEntity::new(Nice::ZERO, FairMode::Normal, 1_000, 1_000);
    current.set_slice_protection(None);

    assert!(protected_current_is_eligible(current, 1_000));
}
