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
fn lock_preempt_exit_reports_only_the_outermost_transition() {
    let mut state = RuntimeGuardState::new();
    state.enter_lock_preempt();
    state.enter_lock_preempt();

    state.exit_lock_preempt();
    assert_eq!(state.preempt.lock_depth, 1);
    state.exit_lock_preempt();
    assert!(state.preempt.is_clear());
}

#[test]
fn scheduler_baton_is_exactly_one_cpu_local_frame() {
    let mut state = RuntimeGuardState::new();
    assert!(state.claim_task_scheduler());
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
#[should_panic(expected = "unbalanced runtime lock preemption guard exit")]
fn lock_exit_cannot_consume_a_scheduler_frame() {
    let mut state = RuntimeGuardState::new();
    assert!(state.claim_task_scheduler());

    state.exit_lock_preempt();
}

#[test]
fn scheduler_frame_cannot_cross_a_live_lock_guard() {
    let mut state = RuntimeGuardState::new();
    state.enter_lock_preempt();

    assert!(!state.claim_task_scheduler());
    assert!(state.claim_preempt_exit_scheduler());
}

#[test]
fn scheduler_frame_cannot_enter_inside_an_ordinary_irq_guard() {
    let mut state = RuntimeGuardState::new();
    state.enter_irq(true);

    assert!(!state.claim_task_scheduler());
}

#[test]
fn owner_cpu_context_requires_irq_pin_or_scheduler_baton() {
    let mut state = RuntimeGuardState::new();
    assert!(!state.owns_cpu_context());

    state.enter_lock_preempt();
    assert!(
        !state.owns_cpu_context(),
        "a lock-local preemption depth cannot stand in for rq ownership"
    );
    state.exit_lock_preempt();

    state.enter_irq(true);
    assert!(state.owns_cpu_context());
    assert!(state.exit_irq("test"));
    assert!(!state.owns_cpu_context());

    assert!(state.claim_task_scheduler());
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
    assert!(state.claim_task_scheduler());
    state.enter_irq(true);

    state.exit_scheduler_preempt("test scheduler frame");
}

#[test]
#[cfg(feature = "fs")]
fn context_guard_state_rejects_sleep_until_every_depth_is_released() {
    let mut state = RuntimeGuardState::new();
    assert!(!state.has_context_guard());

    state.enter_lock_preempt();
    assert!(state.has_context_guard());
    state.exit_lock_preempt();
    assert!(!state.has_context_guard());
}

#[test]
fn initial_context_entry_consumes_the_scheduler_baton() {
    let mut state = RuntimeGuardState::new();
    assert!(state.claim_task_scheduler());

    state.exit_scheduler_preempt("test scheduler frame");
    assert!(state.preempt.is_clear());
}
