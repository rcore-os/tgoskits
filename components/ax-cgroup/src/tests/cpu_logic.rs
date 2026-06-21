//! Unit tests for cpu controller pure logic (weight→nice mapping).

use crate::cpu::weight_to_nice;

#[test]
fn weight_default_maps_to_nice_zero() {
    // cgroup default weight 100 == scheduler load 1024 == nice 0.
    assert_eq!(weight_to_nice(100), 0);
}

#[test]
fn weight_extremes_clamp_to_nice_range() {
    // Max weight is the most favourable (lowest) nice.
    assert_eq!(weight_to_nice(10_000), -20);
    // Min weight is the least favourable (highest) nice.
    assert_eq!(weight_to_nice(1), 19);
}

#[test]
fn weight_monotonic_decreasing_nice() {
    // Higher weight must never yield a higher nice (more weight = less nice).
    let mut prev = weight_to_nice(1);
    for w in [1, 10, 50, 100, 500, 1000, 5000, 10_000] {
        let nice = weight_to_nice(w);
        assert!(
            nice <= prev,
            "weight {w} gave nice {nice}, expected <= {prev}"
        );
        prev = nice;
    }
}

#[test]
fn weight_out_of_range_is_clamped() {
    // Below 1 and above 10000 clamp to the boundary mappings.
    assert_eq!(weight_to_nice(0), 19);
    assert_eq!(weight_to_nice(-100), 19);
    assert_eq!(weight_to_nice(50_000), -20);
}

#[test]
fn weight_in_range_stays_within_nice_bounds() {
    for w in [1, 2, 7, 33, 100, 256, 999, 4096, 10_000] {
        let nice = weight_to_nice(w);
        assert!(
            (-20..=19).contains(&nice),
            "weight {w} -> nice {nice} out of range"
        );
    }
}
