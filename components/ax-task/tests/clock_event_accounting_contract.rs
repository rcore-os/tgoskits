const DEADLINE_FACADE: &str = include_str!("../src/facade/deadline.rs");

#[test]
fn plain_hard_timer_accounts_current_before_callbacks() {
    let runtime_arm = DEADLINE_FACADE
        .find("ClockAccountingKind::RuntimeOnly =>")
        .expect("clock-event accounting must retain the RuntimeOnly plan");
    let runtime_accounting = DEADLINE_FACADE[runtime_arm..]
        .find("system.charge_current_until_with_clock(cpu.as_mut(), 0)?")
        .map(|offset| runtime_arm + offset)
        .expect("RuntimeOnly clock events must charge the interrupted current task");
    let scheduler_arm = DEADLINE_FACADE[runtime_arm..]
        .find("ClockAccountingKind::SchedulerDeadline =>")
        .map(|offset| runtime_arm + offset)
        .expect("clock-event accounting must retain the scheduler-deadline plan");
    let hard_callbacks = DEADLINE_FACADE
        .find("system.service_due_hard_timers(cpu.as_mut(), now, budget)?")
        .expect("clock-event handling must retain hard-timer service");

    assert!(
        runtime_arm < runtime_accounting
            && runtime_accounting < scheduler_arm
            && scheduler_arm < hard_callbacks,
        "current-task accounting must precede hard-timer callbacks"
    );
}

#[test]
fn periodic_tick_publishes_promoted_lazy_work_to_irq_return() {
    let promotion = DEADLINE_FACADE
        .split_once("if periodic_tick")
        .expect("periodic clock events must retain lazy-request promotion")
        .1
        .split_once("// A plain hard-timer callback")
        .expect("lazy promotion must precede clock-event accounting")
        .0;

    assert!(
        promotion.contains("cpu.promote_lazy_reschedule()")
            && promotion.contains("task_runtime::publish_local_scheduler_work()"),
        "a promoted lazy request must set the architecture preemption word observed by IRQ return"
    );
}
