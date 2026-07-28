//! Deterministic close-vs-arm contract for Starry PMU sampling.

#[path = "../src/perf/sampling_lifecycle.rs"]
mod sampling_lifecycle;
#[path = "../src/perf/target.rs"]
mod target;

use sampling_lifecycle::{PmuCloseAction, PmuRunState, PmuStopClaim, SampleRegistration};
use target::PerfCpuId;

#[test]
fn close_after_registry_publish_must_disarm_before_reclaim() {
    let cpu = PerfCpuId::new(1);
    let mut state = PmuRunState::new();
    let arm = state.begin_arm(cpu).unwrap();
    let registration = SampleRegistration::new(cpu, 3, 17);
    state.publish_registration(arm, registration);

    let PmuCloseAction::Stop(lease) = state.begin_close() else {
        panic!("a slot is IRQ-reachable before the legacy running flag is published");
    };
    assert_eq!(lease.owner(), cpu);
    assert_eq!(lease.registration(), Some(registration));
}

#[test]
fn fully_running_generation_is_disarmed_on_its_owner_cpu() {
    let cpu = PerfCpuId::new(2);
    let mut state = PmuRunState::new();
    let arm = state.begin_arm(cpu).unwrap();
    let registration = SampleRegistration::new(cpu, 4, 23);
    state.publish_registration(arm, registration);
    state.finish_arm(arm);

    let PmuCloseAction::Stop(lease) = state.begin_close() else {
        panic!("running registration was not disarmed");
    };
    let observed = lease.registration().unwrap();
    assert_eq!(observed.owner(), cpu);
    assert_eq!(observed.counter(), 4);
    assert_eq!(observed.generation(), 23);
}

#[test]
fn close_request_remains_visible_to_the_switch_out_owner() {
    let cpu = PerfCpuId::new(3);
    let mut state = PmuRunState::new();
    let arm = state.begin_arm(cpu).unwrap();
    state.finish_arm(arm);

    let PmuCloseAction::Stop(lease) = state.begin_close() else {
        panic!("running event must request an owner-CPU stop");
    };
    assert_eq!(
        state.running(),
        Some(lease),
        "switch-out must still claim a close-requested hardware generation"
    );
    assert_eq!(state.claim_schedule_out(), Some(lease));
    state.finish_owner_stop(lease);
    assert_eq!(
        state.claim_requested_stop(lease),
        PmuStopClaim::AlreadyComplete,
        "the affine worker must treat a switch-out winner as a completed fence"
    );
    assert!(state.is_stopping());
}

#[test]
fn disable_stops_one_generation_without_closing_the_event() {
    let cpu = PerfCpuId::new(1);
    let mut state = PmuRunState::new();
    let arm = state.begin_arm(cpu).unwrap();
    state.finish_arm(arm);

    let PmuCloseAction::Stop(lease) = state.begin_disable() else {
        panic!("disable must fence the active generation");
    };
    assert_eq!(
        state.claim_requested_stop(lease),
        PmuStopClaim::Claimed(lease)
    );
    state.finish_owner_stop(lease);
    assert!(!state.is_stopping());
    assert!(
        state.begin_arm(cpu).is_some(),
        "disable must permit re-enable"
    );
}

#[test]
fn failed_owner_stop_can_be_claimed_again() {
    let cpu = PerfCpuId::new(2);
    let mut state = PmuRunState::new();
    let arm = state.begin_arm(cpu).unwrap();
    state.finish_arm(arm);

    let PmuCloseAction::Stop(lease) = state.begin_close() else {
        panic!("close must fence the active generation");
    };
    assert_eq!(
        state.claim_requested_stop(lease),
        PmuStopClaim::Claimed(lease)
    );

    // Model a fixed-CPU worker that claimed the stop but could not complete the
    // architecture operation. Teardown must retain the exact generation and
    // permit a later fd/task release to retry it.
    state.abort_owner_stop(lease);
    assert_eq!(
        state.claim_requested_stop(lease),
        PmuStopClaim::Claimed(lease)
    );
}

#[test]
fn close_upgrades_an_in_flight_disable_to_permanent_teardown() {
    let cpu = PerfCpuId::new(4);
    let mut state = PmuRunState::new();
    let arm = state.begin_arm(cpu).unwrap();
    state.finish_arm(arm);

    let PmuCloseAction::Stop(lease) = state.begin_disable() else {
        panic!("disable must fence the active generation");
    };
    assert_eq!(
        state.claim_requested_stop(lease),
        PmuStopClaim::Claimed(lease)
    );
    assert_eq!(state.begin_close(), PmuCloseAction::Stop(lease));
    state.finish_owner_stop(lease);

    assert!(state.is_stopping());
    assert_eq!(state.begin_close(), PmuCloseAction::AlreadyClosed);
}

#[test]
fn a_stale_lease_cannot_stop_the_next_arm_generation() {
    let cpu = PerfCpuId::new(5);
    let mut state = PmuRunState::new();
    let first_arm = state.begin_arm(cpu).unwrap();
    state.finish_arm(first_arm);
    let PmuCloseAction::Stop(first) = state.begin_disable() else {
        panic!("first disable must return its lease");
    };
    assert_eq!(
        state.claim_requested_stop(first),
        PmuStopClaim::Claimed(first)
    );
    state.finish_owner_stop(first);

    let second_arm = state.begin_arm(cpu).unwrap();
    state.finish_arm(second_arm);
    assert_eq!(state.claim_requested_stop(first), PmuStopClaim::Stale);
}
