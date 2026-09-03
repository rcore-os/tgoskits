use super::*;

#[cfg(feature = "host-test")]
static HOST_CPU_GUARD_TEST: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(feature = "host-test")]
#[test]
fn host_spin_guard_before_runtime_bootstrap_is_noop() {
    let lock = crate::sync::SpinLock::new(());
    let _guard = lock.lock_irqsave();
}

#[cfg(feature = "host-test")]
#[test]
fn scheduler_exit_state_reuses_one_cpu_pin() {
    let _serial = HOST_CPU_GUARD_TEST
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    std::thread::spawn(|| {
        ax_hal::percpu::initialize_host_test_cpu();
        ax_hal::asm::disable_irqs();
        with_guard_state_mut(|state| assert!(state.claim_task_scheduler(0)));
        cpu_local::host_test::reset_register_read_counts();

        finish_scheduler_cpu_transaction(false, "test scheduler frame");

        let reads = cpu_local::host_test::register_read_counts();
        assert_eq!(
            reads.current_context, 1,
            "scheduler exit reuses the published current and reads only preemption ownership"
        );
        assert_eq!(
            reads.binding_observations, 0,
            "scheduler exit must trust switch-time binding publication"
        );
        ax_hal::asm::enable_irqs();
    })
    .join()
    .expect("modeled CPU must finish scheduler exit state");
}

#[cfg(feature = "host-test")]
#[test]
fn scheduler_entry_state_reuses_one_cpu_pin() {
    let _serial = HOST_CPU_GUARD_TEST
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    std::thread::spawn(|| {
        ax_hal::percpu::initialize_host_test_cpu();
        ax_hal::asm::disable_irqs();
        with_current_cpu_pin(cpu_local::release_bootstrap_preemption)
            .expect("modeled task must release bootstrap preemption");
        cpu_local::host_test::reset_register_read_counts();

        let capabilities = claim_scheduler_cpu_state(ax_task::runtime::RuntimeSchedulerEntry::Task)
            .expect("modeled task must claim one scheduler-frame capability snapshot");
        assert_eq!(
            capabilities.status(),
            ax_task::runtime::RuntimeStatus::Success,
            "a claimed scheduler frame must publish a successful capability snapshot"
        );

        let reads = cpu_local::host_test::register_read_counts();
        assert_eq!(
            reads.current_context, 1,
            "scheduler entry reuses the published current and reads only preemption ownership"
        );
        assert_eq!(
            reads.binding_observations, 0,
            "scheduler entry must trust switch-time binding publication"
        );
        with_guard_state_mut(|state| state.exit_scheduler_preempt("test scheduler frame"));
        ax_hal::asm::enable_irqs();
    })
    .join()
    .expect("modeled CPU must finish scheduler entry state");
}

#[cfg(feature = "host-test")]
#[test]
fn irq_pinned_guard_state_read_skips_current_context_reconstruction() {
    let _serial = HOST_CPU_GUARD_TEST
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    std::thread::spawn(|| {
        ax_hal::percpu::initialize_host_test_cpu();
        ax_hal::asm::disable_irqs();
        cpu_local::host_test::reset_register_read_counts();

        assert!(read_state().irq.is_clear());

        let reads = cpu_local::host_test::register_read_counts();
        assert_eq!(reads.cpu_base, 1, "the owner read selects one CPU area");
        assert_eq!(
            reads.current_context, 0,
            "an IRQ-pinned CPU owner must not reconstruct task current"
        );
        ax_hal::asm::enable_irqs();
    })
    .join()
    .expect("modeled CPU must finish the owner-state read");
}

#[cfg(feature = "host-test")]
#[test]
fn final_preempt_exit_reuses_one_cpu_pin_and_one_depth_snapshot() {
    let _serial = HOST_CPU_GUARD_TEST
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    std::thread::spawn(|| {
        ax_hal::percpu::initialize_host_test_cpu();
        ax_hal::asm::disable_irqs();
        assert_eq!(
            current_preempt_depth(),
            1,
            "host CPU bootstrap depth models the retained final exit"
        );
        cpu_local::host_test::reset_register_read_counts();

        assert!(claim_preempt_exit_scheduler(PreemptExitOrigin::Task, true));

        let reads = cpu_local::host_test::register_read_counts();
        assert_eq!(
            reads.current_context, 1,
            "preempt exit reuses the published current and reads only the owned depth"
        );
        with_guard_state_mut(|state| state.exit_scheduler_preempt("modeled preempt exit"));
        ax_hal::asm::enable_irqs();
    })
    .join()
    .expect("modeled CPU must finish the final preempt exit");
}

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

    assert!(!preempt_exit_needs_schedule(
        &state,
        2,
        PreemptExitOrigin::IrqReturn,
        false,
        || {
            irq_queries.set(irq_queries.get() + 1);
            false
        },
    ));
    assert_eq!(
        irq_queries.get(),
        0,
        "a nested NoPreempt drop must not recursively query IRQ state"
    );
}

#[test]
fn final_preempt_exit_does_not_requery_the_reschedule_endpoint() {
    let state = RuntimeGuardState::new();

    assert!(preempt_exit_needs_schedule(
        &state,
        1,
        PreemptExitOrigin::Task,
        true,
        || false,
    ));
}

#[test]
fn task_preempt_exit_defers_while_hardware_irqs_are_disabled() {
    let state = RuntimeGuardState::new();

    assert!(!preempt_exit_needs_schedule(
        &state,
        1,
        PreemptExitOrigin::Task,
        false,
        || false,
    ));
}

#[test]
fn explicit_irq_return_may_schedule_with_hardware_irqs_disabled() {
    let state = RuntimeGuardState::new();

    assert!(preempt_exit_needs_schedule(
        &state,
        1,
        PreemptExitOrigin::IrqReturn,
        false,
        || false,
    ));
}

#[test]
fn nested_irq_exit_does_not_reenter_context_queries() {
    use core::cell::Cell;

    let mut state = RuntimeGuardState::new();
    state.enter_irq(true);
    state.enter_irq(false);
    let reschedule_queries = Cell::new(0);

    assert!(!irq_guard_exit_needs_schedule(&state, 0, || {
        reschedule_queries.set(reschedule_queries.get() + 1);
        false
    },));
    assert_eq!(
        reschedule_queries.get(),
        0,
        "a nested IRQ guard drop must not recursively query scheduler state"
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
fn pending_preempt_exit_has_no_preemptible_gap_before_scheduler_entry() {
    let mut state = RuntimeGuardState::new();

    assert!(state.claim_preempt_exit_scheduler(1));
    assert_eq!(
        state.preempt.scheduler_baton,
        SchedulerBatonState::PreemptEntry,
        "the final depth must become a distinct preclaimed baton before release"
    );
    assert!(!state.preempt.has_active_scheduler_baton());
    assert!(state.owns_cpu_context());

    assert!(state.enter_preclaimed_scheduler(0));
    assert!(state.preempt.has_active_scheduler_baton());
    state.exit_scheduler_preempt("test preempt scheduler frame");
    assert!(state.preempt.is_clear());
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
