#[path = "../src/system/task_system/root_domain/fair_nohz_state.rs"]
mod fair_nohz_state;

use fair_nohz_state::{FairNoHzClaim, FairNoHzPhase, FairNoHzState, FairNoHzTransition};

#[test]
fn withdrawing_a_claimed_balancer_retargets_without_stale_completion_clobbering_it() {
    let source = 0;
    let first_idle = 1;
    let second_idle = 2;
    let claim = FairNoHzClaim {
        balancer: first_idle,
        generation: 1,
    };
    let mut state = FairNoHzState {
        requested_generation: 1,
        scan_generation: 1,
        source,
        cursor: None,
        phase: FairNoHzPhase::Claimed(first_idle),
    };

    let retargeted = state.withdraw_balancer(first_idle, |cursor, selected_source| {
        assert_eq!(cursor, Some(first_idle));
        assert_eq!(selected_source, source);
        Some(second_idle)
    });

    assert_eq!(retargeted.target, Some(second_idle));
    assert_eq!(state.phase, FairNoHzPhase::Published(second_idle));
    assert_eq!(
        state.finish_balancer(claim, false, true, |_, _| {
            panic!("a stale completion must not select another ILB owner")
        }),
        FairNoHzTransition::UNCHANGED,
        "completion from the withdrawn owner must be stale"
    );
    assert_eq!(state.phase, FairNoHzPhase::Published(second_idle));
}

#[test]
fn stale_completion_cannot_finish_a_later_generation_on_the_same_cpu() {
    let cpu = 1;
    let stale_claim = FairNoHzClaim {
        balancer: cpu,
        generation: 1,
    };
    let mut state = FairNoHzState {
        requested_generation: 2,
        scan_generation: 2,
        source: 0,
        cursor: None,
        phase: FairNoHzPhase::Claimed(cpu),
    };

    assert_eq!(
        state.finish_balancer(stale_claim, true, true, |_, _| {
            panic!("a stale generation must not scan for another owner")
        }),
        FairNoHzTransition::UNCHANGED
    );
    assert_eq!(state.phase, FairNoHzPhase::Claimed(cpu));
}

#[test]
fn failed_delivery_retargets_only_the_published_owner() {
    let mut state = FairNoHzState {
        requested_generation: 1,
        scan_generation: 1,
        source: 0,
        cursor: None,
        phase: FairNoHzPhase::Published(1),
    };

    let transition = state.retarget_failed_delivery(1, |cursor, source| {
        assert_eq!(cursor, Some(1));
        assert_eq!(source, 0);
        Some(2)
    });

    assert_eq!(transition.target, Some(2));
    assert_eq!(state.phase, FairNoHzPhase::Published(2));
    assert!(state.claim_balancer(1).is_none());
    assert_eq!(
        state.claim_balancer(2),
        Some(FairNoHzClaim {
            balancer: 2,
            generation: 1,
        })
    );
}
