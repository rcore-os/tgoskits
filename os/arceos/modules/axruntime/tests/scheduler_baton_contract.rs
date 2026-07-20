//! Source-level contract for the CPU-local scheduler baton state machine.

const GUARD: &str = include_str!("../src/guard.rs");
const TASK_RUNTIME: &str = include_str!("../src/task.rs");

fn source_section<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
    let (_, tail) = source
        .split_once(start)
        .unwrap_or_else(|| panic!("missing source section start: {start}"));
    tail.split_once(end)
        .unwrap_or_else(|| panic!("missing source section end: {end}"))
        .0
}

#[test]
fn raw_context_switch_transfers_an_active_baton_before_leaving_the_stack() {
    for state in ["Active", "Transferred", "Finished"] {
        assert!(
            GUARD.contains(state),
            "scheduler baton must represent the {state} state explicitly",
        );
    }
    assert!(
        TASK_RUNTIME.contains("crate::guard::transfer_scheduler_switch_baton();"),
        "the runtime must publish baton transfer before the raw context switch",
    );
}

#[test]
fn final_preempt_exit_finishes_the_baton_before_restoring_saved_irqs() {
    let branch = source_section(GUARD, "if must_schedule {", "state.exit_lock_preempt();");
    let schedule = branch
        .find("schedule_current_cpu_from_preempt_exit")
        .expect("the final preempt depth must transfer directly to the scheduler");
    let completed = branch
        .find("assert_preempt_exit_completed();")
        .expect("the resumed continuation must verify that its baton is finished");
    let restore = branch
        .find("irq_owner.restore_saved_irq_state();")
        .expect("the original guard continuation must restore its saved IRQ state");
    assert!(
        schedule < completed && completed < restore,
        "IRQ restoration must remain continuation-local and happen only after baton completion",
    );
    assert!(
        !branch[..restore].contains("enable_irqs"),
        "raw IRQs must stay disabled through scheduler entry and baton completion",
    );
    assert!(
        GUARD.contains(
            "RuntimeSchedulerEntry::PreemptExit | RuntimeSchedulerEntry::IrqReturn => \
             !irqs_enabled"
        ),
        "both guard-exit entries must arrive with raw IRQs disabled",
    );
    assert!(
        GUARD.contains("struct PreemptExitIrqOwner"),
        "saved IRQ state must have an owner distinct from the CPU-local scheduler baton",
    );
}

#[test]
fn unsafe_schedule_context_reports_the_complete_cpu_local_snapshot() {
    for field in [
        "raw_irqs_enabled",
        "hard_irq",
        "irq_depth",
        "preempt_lock_depth",
        "scheduler_baton",
    ] {
        assert!(
            GUARD.contains(field),
            "unsafe scheduling diagnostics must retain the `{field}` state",
        );
    }
    assert!(
        GUARD.contains("ScheduleContextSnapshot"),
        "schedule-context validation must classify one typed snapshot",
    );
    assert!(
        GUARD.contains("report_unsafe_schedule_context"),
        "an unsafe context must emit one fixed-capacity, allocation-free diagnostic",
    );
}
