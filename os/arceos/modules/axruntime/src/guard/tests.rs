use super::*;

#[test]
fn nested_irq_exits_restore_only_the_outer_state() {
    let mut state = RuntimeGuardState::new();
    state.enter_irq(true);
    state.enter_irq(false);

    assert!(!state.exit_irq("test"));
    assert!(state.exit_irq("test"));
}

#[test]
fn disabled_outer_irq_state_stays_disabled() {
    let mut state = RuntimeGuardState::new();
    state.enter_irq(false);

    assert!(!state.exit_irq("test"));
}

#[test]
fn final_task_irq_guard_converts_directly_into_scheduler_baton() {
    let mut state = RuntimeGuardState::new();
    state.enter_irq(true);

    assert!(state.local_scheduler_work_is_self_serviced(0));
    assert!(state.claim_irq_exit_scheduler(0));
    assert!(state.irq.is_clear());
    assert!(state.preempt.has_active_scheduler_baton());

    state.exit_scheduler_preempt("test scheduler frame");
    assert!(state.preempt.is_clear());
}

#[test]
fn disabled_task_irq_guard_cannot_promise_a_local_scheduler_entry() {
    let mut state = RuntimeGuardState::new();
    state.enter_irq(false);

    assert!(!state.local_scheduler_work_is_self_serviced(0));
    assert!(!state.claim_irq_exit_scheduler(0));
    assert!(!state.exit_irq("test"));
}

#[test]
fn nested_preempt_exit_does_not_reenter_context_queries() {
    use core::cell::Cell;

    let state = RuntimeGuardState::new();
    let irq_queries = Cell::new(0);
    let reschedule_queries = Cell::new(0);

    assert!(!preempt_exit_needs_schedule(
        &state,
        2,
        true,
        false,
        || {
            irq_queries.set(irq_queries.get() + 1);
            false
        },
        || {
            reschedule_queries.set(reschedule_queries.get() + 1);
            false
        },
    ));
    assert_eq!(
        (irq_queries.get(), reschedule_queries.get()),
        (0, 0),
        "a nested NoPreempt drop must not recursively query IRQ or scheduler state"
    );
}

#[test]
fn nested_irq_exit_does_not_reenter_context_queries() {
    use core::cell::Cell;

    let mut state = RuntimeGuardState::new();
    state.enter_irq(true);
    state.enter_irq(false);
    let irq_queries = Cell::new(0);
    let reschedule_queries = Cell::new(0);

    assert!(!irq_guard_exit_needs_schedule(
        &state,
        0,
        || {
            irq_queries.set(irq_queries.get() + 1);
            false
        },
        || {
            reschedule_queries.set(reschedule_queries.get() + 1);
            false
        },
    ));
    assert_eq!(
        (irq_queries.get(), reschedule_queries.get()),
        (0, 0),
        "a nested IRQ guard drop must not recursively query IRQ or scheduler state"
    );
}

#[test]
fn scheduler_baton_is_exactly_one_cpu_local_frame() {
    let mut state = RuntimeGuardState::new();
    assert!(state.claim_task_scheduler(0));
    assert!(state.preempt.has_one_scheduler_frame());
    assert_eq!(state.preempt.scheduler_baton, SchedulerBatonState::Active);

    state.transfer_scheduler_preempt();
    assert_eq!(
        state.preempt.scheduler_baton,
        SchedulerBatonState::Transferred
    );

    state.exit_scheduler_preempt("test scheduler frame");
    assert!(state.preempt.is_clear());
    assert_eq!(state.preempt.scheduler_baton, SchedulerBatonState::Finished);
}

#[test]
fn preempt_exit_cannot_replace_an_active_scheduler_frame() {
    let mut state = RuntimeGuardState::new();
    assert!(state.claim_task_scheduler(0));

    assert!(!state.claim_preempt_exit_scheduler(1));
}

#[test]
fn scheduler_frame_cannot_cross_a_live_lock_guard() {
    let mut state = RuntimeGuardState::new();

    assert!(!state.claim_task_scheduler(1));
    assert!(state.claim_preempt_exit_scheduler(1));
}

#[test]
fn scheduler_frame_cannot_enter_inside_an_ordinary_irq_guard() {
    let mut state = RuntimeGuardState::new();
    state.enter_irq(true);

    assert!(!state.claim_task_scheduler(0));
}

#[test]
fn owner_cpu_context_requires_irq_pin_or_scheduler_baton() {
    let mut state = RuntimeGuardState::new();
    assert!(!state.owns_cpu_context());

    assert!(
        !state.owns_cpu_context(),
        "a lock-local preemption depth cannot stand in for rq ownership"
    );

    state.enter_irq(true);
    assert!(state.owns_cpu_context());
    assert!(state.exit_irq("test"));
    assert!(!state.owns_cpu_context());

    assert!(state.claim_task_scheduler(0));
    assert!(state.owns_cpu_context());
    state.transfer_scheduler_preempt();
    assert!(state.owns_cpu_context());
    state.exit_scheduler_preempt("test scheduler frame");
    assert!(!state.owns_cpu_context());
}

#[test]
#[should_panic(expected = "test scheduler frame exited with live IRQ guard depth=1")]
fn scheduler_frame_cannot_cross_a_live_irq_guard() {
    let mut state = RuntimeGuardState::new();
    assert!(state.claim_task_scheduler(0));
    state.enter_irq(true);

    state.exit_scheduler_preempt("test scheduler frame");
}

#[test]
#[cfg(feature = "fs")]
fn context_guard_state_rejects_sleep_until_every_depth_is_released() {
    let mut state = RuntimeGuardState::new();
    assert!(!state.has_context_guard(0));

    assert!(state.has_context_guard(1));
    assert!(!state.has_context_guard(0));
}

#[test]
fn initial_context_entry_consumes_the_scheduler_baton() {
    let mut state = RuntimeGuardState::new();
    assert!(state.claim_task_scheduler(0));

    state.exit_scheduler_preempt("test scheduler frame");
    assert!(state.preempt.is_clear());
}
