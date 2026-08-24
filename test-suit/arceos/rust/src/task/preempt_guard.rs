use std::os::arceos::modules::{ax_hal, ax_runtime, ax_task::task_test_hooks};

pub fn run() -> crate::TestResult {
    assert!(
        ax_hal::asm::irqs_enabled(),
        "ordinary preemption regression must start in task context"
    );
    ax_runtime::reset_ordinary_preempt_exit_slow_path_count();

    task_test_hooks::exercise_nested_preempt_guards();

    assert_eq!(
        ax_runtime::take_ordinary_preempt_exit_slow_path_count(),
        0,
        "nested and non-pending preemption exits must stay on Linux's decrement-only path"
    );
    assert!(
        ax_hal::asm::irqs_enabled(),
        "ordinary preemption exits must preserve task-context IRQ state"
    );
    Ok(())
}
