use std::os::arceos::modules::{ax_hal, ax_runtime, ax_task::task_test_hooks};

pub fn run() -> crate::TestResult {
    assert!(
        ax_hal::asm::irqs_enabled(),
        "ordinary preemption regression must start in task context"
    );
    ax_runtime::reset_ordinary_preempt_exit_slow_path_count();

    task_test_hooks::exercise_nested_preempt_guard_inner_exit(|| {
        assert_eq!(
            ax_runtime::take_ordinary_preempt_exit_slow_path_count(),
            0,
            "a nested preemption exit must stay on Linux's decrement-only path"
        );
    });
    assert!(
        ax_hal::asm::irqs_enabled(),
        "the final ordinary preemption exit must restore task-context IRQ state"
    );
    Ok(())
}
