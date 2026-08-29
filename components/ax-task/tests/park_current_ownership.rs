const PARK_EXIT: &str = include_str!("../src/system/task_system/park_exit.rs");
const DEADLINE: &str = include_str!("../src/facade/deadline.rs");

#[test]
fn current_park_state_operations_borrow_the_scheduler_owned_current() {
    assert!(
        PARK_EXIT.contains(
            "pub(crate) fn prepare_current_park(\n        &self,\n        current: &ThreadCore,",
        ),
        "park preparation must borrow Linux-style scheduler-owned current state",
    );
    let cancel = PARK_EXIT
        .split_once("pub(crate) fn cancel_current_park(")
        .expect("current park cancellation must remain present")
        .1
        .split_once("\n    /// Validates all fallible current-thread exit prerequisites")
        .expect("current park cancellation must remain a focused operation")
        .0;
    assert!(
        cancel.contains("current: &ThreadCore,"),
        "park cancellation must borrow Linux-style scheduler-owned current state",
    );
    assert!(
        DEADLINE.contains("pub(crate) fn arm_current_park_deadline(\n    thread: &ThreadCore,",),
        "deadline registration must borrow the prepared current owner",
    );
    assert!(
        DEADLINE.contains("pub(crate) fn cancel_current_park_deadline(\n    thread: &ThreadCore,",),
        "deadline cancellation must borrow the prepared current owner",
    );
    assert!(
        !PARK_EXIT.contains("let core = Arc::clone(current);"),
        "state-only park operations must not manufacture temporary Arc ownership",
    );
    assert!(
        !DEADLINE.contains("let thread = Arc::clone(&self.thread);"),
        "arming a prepared park deadline must not clone its existing owner",
    );
}
