const WAKE: &str = include_str!("../src/system/task_system/dispatch/wake.rs");

#[test]
fn task_system_wake_does_not_repeat_thread_handle_exit_check() {
    let wake_thread = WAKE
        .split_once("fn wake_thread(\n")
        .expect("task-system wake helper must remain present")
        .1
        .split_once("\n    /// Delivers one wait-queue notification")
        .expect("task-system wake helper must remain focused")
        .0;

    assert!(
        !wake_thread.contains("if core.state() == ThreadState::Exited"),
        "the task scheduler lock is the authoritative exit check; keep wake to one state \
         publication"
    );
}
