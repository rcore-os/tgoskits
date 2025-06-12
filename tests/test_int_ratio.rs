// test_int_ratio.rs

// The user prompt asked for `use int_ratio::*`, which is idiomatic for an
// integration test located in the `tests/` directory of a crate.
use int_ratio::Ratio;

#[test]
fn test_equivalence_of_simplified_ratios() {
    // Ratios that simplify to the same fraction should be equal.
    // The PartialEq implementation compares the calculated `mult` and `shift`.
    let r1 = Ratio::new(1, 2); // 0.5
    let r2 = Ratio::new(2, 4); // 0.5
    let r3 = Ratio::new(50, 100); // 0.5
    assert_eq!(r1, r2);
    assert_eq!(r2, r3);

    // A different ratio should not be equal.
    let r4 = Ratio::new(1, 3);
    assert_ne!(r1, r4);

    // Test a more complex simplification
    let r5 = Ratio::new(6, 8); // 3/4
    let r6 = Ratio::new(3, 4); // 3/4
    assert_eq!(r5, r6);
}

#[test]
fn test_inverse_operation() {
    // Test the inverse of a standard ratio.
    let r1 = Ratio::new(2, 5);
    let r1_inv = Ratio::new(5, 2);
    assert_eq!(r1.inverse(), r1_inv);

    // The inverse of the inverse should be the original ratio.
    assert_eq!(r1.inverse().inverse(), r1);

    // `Ratio::zero()` is a special case where inverse returns itself.
    let zero = Ratio::zero();
    assert_eq!(zero.inverse(), zero);
}

#[test]
fn test_multiplication_trunc_vs_round() {
    // Test case where result is just under 0.5, so round() rounds down.
    // 10 * 1/3 = 3.33...
    let ratio_one_third = Ratio::new(1, 3);
    assert_eq!(ratio_one_third.mul_trunc(10), 3); // trunc(3.33...) = 3
    assert_eq!(ratio_one_third.mul_round(10), 3); // round(3.33...) = 3

    // Test case where result is exactly 0.5, so round() rounds up.
    // 99 * 1/2 = 49.5
    let ratio_one_half = Ratio::new(1, 2);
    assert_eq!(ratio_one_half.mul_trunc(99), 49); // trunc(49.5) = 49
    assert_eq!(ratio_one_half.mul_round(99), 50); // round(49.5) = 50

    // Test case where result is over 0.5, so round() rounds up.
    // 100 * 2/3 = 66.66...
    let ratio_two_thirds = Ratio::new(2, 3);
    assert_eq!(ratio_two_thirds.mul_trunc(100), 66); // trunc(66.66...) = 66
    assert_eq!(ratio_two_thirds.mul_round(100), 67); // round(66.66...) = 67
}

#[test]
fn test_zero_ratio_behaviors() {
    // `Ratio::zero()` (0/0) and `Ratio::new(0, x)` should be equal in value.
    let z1 = Ratio::zero();
    let z2 = Ratio::new(0, 100);
    let z3 = Ratio::new(0, u32::MAX);
    assert_eq!(z1, z2);
    assert_eq!(z2, z3);

    // They should all result in 0 when multiplying.
    assert_eq!(z1.mul_trunc(12345), 0);
    assert_eq!(z2.mul_round(u64::MAX), 0);
    assert_eq!(z3.mul_trunc(1), 0);
}

#[test]
#[should_panic]
fn test_panic_on_new_with_zero_denominator() {
    // `Ratio::new` should panic if denominator is 0 but numerator is not.
    let _ = Ratio::new(1, 0);
}

#[test]
#[should_panic]
fn test_panic_on_inverse_of_regular_zero() {
    // The inverse of a ratio `0/x` is `x/0`, which should panic.
    // This is different from `Ratio::zero()` which is `0/0`.
    let regular_zero = Ratio::new(0, 100);
    let _ = regular_zero.inverse(); // This attempts to create Ratio::new(100, 0).
}

#[test]
fn test_max_values() {
    // Ratio of 1
    let ratio_one = Ratio::new(u32::MAX, u32::MAX);
    assert_eq!(ratio_one.mul_trunc(1000), 1000);
    assert_eq!(ratio_one.mul_round(1000), 1000);

    // Ratio > 1
    let ratio_large = Ratio::new(u32::MAX, 1);
    assert_eq!(ratio_large.mul_trunc(2), 2 * u32::MAX as u64);

    // Ratio < 1 using large numbers
    let ratio_almost_one = Ratio::new(u32::MAX - 1, u32::MAX);

    assert_eq!(
        ratio_almost_one.mul_round(u32::MAX as u64),
        (u32::MAX - 1) as u64
    );

    assert_eq!(
        ratio_almost_one.mul_trunc(u32::MAX as u64),
        (u32::MAX - 1) as u64
    );
}
