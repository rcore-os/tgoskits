//! Unit tests for cpu controller pure logic (weight→nice mapping +
//! cpu.max bandwidth state machine).

use crate::cpu::{BandwidthState, weight_to_nice};

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

// ── cpu.max bandwidth state machine ──────────────────────────────────

use core::sync::atomic::Ordering;

#[test]
fn bandwidth_no_quota_never_throttles() {
    let bw = BandwidthState::new();
    // Default quota -1 (unlimited): consume never reports throttling.
    assert!(!bw.has_quota());
    assert!(!bw.consume(1_000_000));
    assert!(!bw.is_throttled());
}

#[test]
fn bandwidth_consume_until_quota_throttles() {
    let bw = BandwidthState::new();
    bw.quota.store(100_000, Ordering::Release);
    assert!(bw.has_quota());

    // Below quota: not throttled.
    assert!(!bw.consume(40_000));
    assert!(!bw.is_throttled());
    // Crossing the quota reports throttling and flips the state.
    assert!(bw.consume(60_000));
    assert!(bw.is_throttled());
    assert_eq!(bw.nr_throttled.load(Ordering::Acquire), 1);
}

#[test]
fn bandwidth_reset_period_clears_consumption() {
    let bw = BandwidthState::new();
    bw.quota.store(50_000, Ordering::Release);
    assert!(bw.consume(50_000));
    assert!(bw.is_throttled());

    bw.reset_period();
    // After reset: consumption cleared, no longer throttled, periods counted.
    assert!(!bw.is_throttled());
    assert_eq!(bw.nr_periods.load(Ordering::Acquire), 1);
    // A fresh budget is available again.
    assert!(!bw.consume(10_000));
}
