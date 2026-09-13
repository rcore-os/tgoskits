use super::*;

#[test]
fn base_weight_does_not_need_vruntime_scaling() {
    assert!(!weighted_delta_needs_scaling(BASE_WEIGHT as u32));
}

#[test]
fn higher_weight_accumulates_less_vruntime() {
    let mut favored = FairEntity::new(Nice::new(-5).unwrap(), FairMode::Normal, 1_000, 0);
    let mut default = FairEntity::new(Nice::ZERO, FairMode::Normal, 1_000, 0);
    favored.charge(1_000, 0);
    default.charge(1_000, 0);
    assert!(favored.vruntime() < default.vruntime());
}

#[test]
fn virtual_deadline_stays_fixed_until_the_service_request_finishes() {
    let mut entity = FairEntity::new(Nice::ZERO, FairMode::Normal, 1_000, 10_000);
    let deadline = entity.virtual_deadline();

    entity.charge(250, 10_000);

    assert_eq!(entity.virtual_deadline(), deadline);
}

#[test]
fn weighted_request_expires_at_its_virtual_deadline() {
    let mut entity = FairEntity::new(Nice::new(-5).unwrap(), FairMode::Normal, 10_013, 0);
    let deadline = entity.virtual_deadline();

    assert!(!entity.charge(10_012, 0));
    assert_eq!(entity.vruntime(), deadline - 1);
    assert!(
        !entity.charge(1, 0),
        "physical request exhaustion must not renew before vruntime reaches deadline"
    );
    assert_eq!(entity.virtual_deadline(), deadline);
    assert!(entity.charge(4, 0));
    assert!(virtual_after(entity.virtual_deadline(), deadline));
}

#[test]
fn run_to_parity_protects_the_shortest_competing_slice() {
    let mut entity = FairEntity::new(Nice::ZERO, FairMode::Normal, 100_000, 10_000);

    entity.set_slice_protection(Some(25_000));

    assert!(entity.slice_is_protected());
    assert_eq!(entity.finish_runtime_deadline_delta_ns(0), 100_000);
    entity.charge(25_000, 0);
    assert!(!entity.slice_is_protected());
    assert_eq!(entity.finish_runtime_deadline_delta_ns(0), 75_000);
}

#[test]
fn fair_hrtick_tracks_request_deadline_not_run_to_parity_protection() {
    let mut entity = FairEntity::new(Nice::ZERO, FairMode::Normal, 100_000, 10_000);

    entity.set_slice_protection(Some(25_000));

    assert_eq!(
        entity.finish_runtime_deadline_delta_ns(0),
        100_000,
        "Linux hrtick expires at the EEVDF request deadline, not vprot"
    );
}

#[test]
fn fair_hrtick_clamps_a_sub_ten_microsecond_deadline_like_linux() {
    let entity = FairEntity::new(Nice::ZERO, FairMode::Normal, 1, 0);

    assert_eq!(entity.finish_runtime_deadline_delta_ns(0), 10_000);
}

#[test]
fn fair_hrtick_clamps_a_zero_deadline_like_linux() {
    assert_eq!(finish_hrtick_delta_ns(0, 0), 10_000);
}

#[test]
fn fair_hrtick_converts_the_virtual_deadline_back_to_physical_time() {
    let entity = FairEntity::new(Nice::new(-5).unwrap(), FairMode::Normal, 10_013, 0);

    assert_eq!(entity.virtual_deadline(), 3_285);
    assert_eq!(entity.runtime_deadline_delta_ns(), 10_012);
    assert_eq!(entity.finish_runtime_deadline_delta_ns(0), 10_012);
}

#[test]
fn fair_hrtick_compensates_for_irq_utilization_after_weight_conversion() {
    let entity = FairEntity::new(Nice::new(-5).unwrap(), FairMode::Normal, 10_013, 0);

    assert_eq!(entity.finish_runtime_deadline_delta_ns(256), 13_346);
}

#[test]
fn initial_entity_enters_competition_with_half_a_service_request() {
    let mut entity = FairEntity::new(Nice::ZERO, FairMode::Normal, 1_000, 0);

    entity.place_after_activation(10_000, 0).unwrap();

    assert_eq!(entity.service_request_ns(), 1_000);
    assert_eq!(entity.runtime_deadline_delta_ns(), 500);
    assert_eq!(entity.virtual_deadline(), 10_500);
}

#[test]
fn reconfigure_preserves_lag_across_virtual_time_wrap() {
    let entity = FairEntity::test_state(Nice::ZERO, FairMode::Normal, u64::MAX - 50, u64::MAX - 49);

    let reconfigured = entity.reconfigure(Nice::ZERO, FairMode::Normal, 20, 100);

    assert_eq!(reconfigured.vruntime(), 29);
    assert_eq!(reconfigured.virtual_deadline(), 30);
}

#[test]
fn reconfigure_rescales_the_linux_relative_deadline() {
    let mut entity = FairEntity::new(Nice::ZERO, FairMode::Normal, 1_000, 100);
    assert!(!entity.charge(250, 0));

    let reconfigured = entity.reconfigure(Nice::new(-5).unwrap(), FairMode::Normal, 500, 1_000);

    assert_eq!(reconfigured.vruntime(), 951);
    assert_eq!(reconfigured.virtual_deadline(), 1_196);
}

#[test]
fn reconfigure_uses_the_destination_policy_weight() {
    let source = FairEntity::test_state(Nice::ZERO, FairMode::Normal, 90, 91);

    let idle = source.reconfigure(Nice::ZERO, FairMode::Idle, 100, 100);

    assert_eq!(idle.mode, FairMode::Idle);
    assert_eq!(idle.nice, Nice::ZERO);
    assert_eq!(
        idle.weight(),
        crate::sched::SchedulePolicy::IDLE_POLICY_WEIGHT
    );
    assert_eq!(
        virtual_delta(100, idle.vruntime()),
        i64::from(Nice::ZERO.weight()) * 10
            / i64::from(crate::sched::SchedulePolicy::IDLE_POLICY_WEIGHT),
        "Normal-to-Idle reweighting must use WEIGHT_IDLEPRIO as the destination weight"
    );
}

#[test]
fn wakeup_deadline_comparison_survives_virtual_time_wrap() {
    let woken = FairEntity::test_state(Nice::ZERO, FairMode::Normal, u64::MAX - 30, u64::MAX - 20);
    let current = FairEntity::test_state(Nice::ZERO, FairMode::Normal, 0, 5);

    assert!(woken.deadline_precedes(current));
}

#[test]
fn earlier_eevdf_deadline_is_not_hidden_by_legacy_wakeup_granularity() {
    let woken = FairEntity::test_state(Nice::ZERO, FairMode::Normal, 1_000, 1_500);
    let current = FairEntity::test_state(Nice::ZERO, FairMode::Normal, 2_000, 3_000);

    assert!(woken.deadline_precedes(current));
}

#[test]
fn forwarded_wake_keeps_sleep_placement_instead_of_an_active_deadline() {
    let mut entity = FairEntity::test_state(Nice::ZERO, FairMode::Normal, 900, 950);
    entity.capture_sleep_lag(1_000, entity.service_request_ns(), 1_000);

    entity
        .place_after_transfer(2_000, u64::from(Nice::ZERO.weight()))
        .unwrap();

    assert_eq!(
        (entity.vruntime(), entity.virtual_deadline()),
        (1_800, 1_801)
    );
    assert_eq!(
        entity.runtime_deadline_delta_ns(),
        entity.service_request_ns()
    );
}

#[test]
fn sleep_lag_is_bounded_by_the_linux_rq_max_slice() {
    let mut entity = FairEntity::new(Nice::ZERO, FairMode::Normal, 100, 0);
    let rq_max_slice_ns = 1_000;
    let timing_granularity_ns = 10;

    entity.capture_sleep_lag(10_000, rq_max_slice_ns, timing_granularity_ns);

    assert_eq!(
        entity.placement,
        FairPlacement::Sleeping {
            virtual_lag: weighted_delta(rq_max_slice_ns + timing_granularity_ns, entity.weight(),)
                as i64,
        }
    );
}
