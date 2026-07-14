//! Source contract for synchronous affinity changes of the calling thread.

const SCHEDULE: &str = include_str!("../src/syscall/task/schedule.rs");

#[test]
fn current_thread_affinity_uses_the_synchronous_scheduler_commit() {
    let body = SCHEDULE
        .split_once("pub fn sys_sched_setaffinity")
        .expect("sched_setaffinity implementation must exist")
        .1
        .split_once("pub fn sys_sched_getscheduler")
        .expect("sched_setaffinity implementation must remain domain focused")
        .0;

    assert!(
        body.contains("scheduler::set_current_thread_affinity"),
        "the current thread must not return until an excluding affinity mask has migrated it"
    );
    assert!(
        body.contains("current().as_thread().tid()"),
        "pid 0 and an explicit current TID must select the synchronous path"
    );
}
