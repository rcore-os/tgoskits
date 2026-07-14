//! Source-level contract for the CPU-local scheduler baton state machine.

const GUARD: &str = include_str!("../src/guard.rs");
const TASK_RUNTIME: &str = include_str!("../src/task.rs");

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
fn final_preempt_exit_transfers_the_baton_before_irqs_can_be_reenabled() {
    let branch = GUARD
        .split("if must_schedule {")
        .nth(1)
        .and_then(|tail| tail.split("return;").next())
        .expect("preempt-exit scheduling branch must remain explicit");
    assert!(
        !branch.contains("enable_irqs"),
        "the final preempt depth must become the scheduler baton while raw IRQs stay disabled",
    );
    assert!(
        GUARD.contains(
            "RuntimeSchedulerEntry::PreemptExit | RuntimeSchedulerEntry::IrqReturn => \
             !irqs_enabled"
        ),
        "both guard-exit entries must arrive with raw IRQs disabled",
    );
}
