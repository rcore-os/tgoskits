//! Source-level contracts for clone-time resource construction and publication.

const CLONE: &str = include_str!("../src/syscall/task/clone.rs");
const ENTRY: &str = include_str!("../src/entry.rs");
const TASK: &str = include_str!("../src/task/mod.rs");

#[test]
fn process_image_snapshot_drops_each_guard_before_process_construction() {
    let snapshot = TASK
        .split_once("pub(crate) fn image_snapshot(&self) -> ProcessImage")
        .expect("ProcessData must expose an owned image snapshot boundary")
        .1
        .split_once("\n    }")
        .expect("image snapshot must have a bounded function body")
        .0;

    for field in ["exe_path", "cmdline", "auxv"] {
        assert!(
            snapshot.contains(&format!("let {field} = self.{field}.read().clone();")),
            "{field} must be cloned in its own statement so its spin guard drops immediately"
        );
    }

    let snapshot_call = CLONE
        .find("old_proc_data.image_snapshot()")
        .expect("clone must finish the image snapshot before calling ProcessData::new");
    let constructor = CLONE
        .find("ProcessData::new(")
        .expect("clone must construct child process data");
    assert!(
        snapshot_call < constructor,
        "no parent image lock guard may survive into ProcessData::new"
    );
}

#[test]
fn fresh_process_scope_is_initialized_without_publishing_a_scope_guard() {
    assert!(
        !CLONE.contains("proc_data.scope.write()"),
        "a fresh unpublished ProcessData must not enter the active-scope writer"
    );
    let scope_prepare = CLONE
        .find("PreparedProcessScope::from_resources")
        .expect("clone must assemble inherited FD and FS owners into one prepared scope");
    let process_create = CLONE
        .find("ProcessData::new(")
        .expect("clone must create ProcessData");
    assert!(scope_prepare < process_create);
    assert!(!CLONE.contains("scope_cell_mut_unpublished"));
}

#[test]
fn bootstrap_scope_uses_the_same_unpublished_initialization_boundary() {
    assert!(
        !ENTRY.contains("proc.scope.write()"),
        "bootstrap must not publish a fresh process scope merely to initialize stdio"
    );
    assert!(ENTRY.contains("PreparedProcessScope::prepare_init"));
    assert!(!ENTRY.contains("scope_cell_mut_unpublished"));
}
