#[path = "../src/task/cgroup_exit_invariant.rs"]
mod cgroup_exit_invariant;

#[test]
#[should_panic(expected = "cgroup task charge must be released")]
fn cgroup_exit_failure_is_fatal_after_thread_retirement() {
    cgroup_exit_invariant::enforce(Err(ax_cgroup::CgroupError::NoSuchProcess));
}
