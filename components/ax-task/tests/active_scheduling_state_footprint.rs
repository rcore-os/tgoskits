//! Structural regression for scheduler-state ownership across hot rq paths.

#![cfg(feature = "task-test-hooks")]

#[test]
fn active_scheduling_state_is_a_stable_one_word_owner_token() {
    assert_eq!(
        ax_task::task_test_hooks::active_scheduling_state_footprint(),
        core::mem::size_of::<usize>(),
        "block/wake/switch ownership transitions must not copy the complete scheduler entity",
    );
}
