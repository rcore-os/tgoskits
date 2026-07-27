//! Source-level Linux contract for per-thread nice ownership.

const TASK: &str = include_str!("../src/task/mod.rs");
const SCHEDULE: &str = include_str!("../src/syscall/task/schedule.rs");
const CLONE: &str = include_str!("../src/syscall/task/clone.rs");
const STAT: &str = include_str!("../src/task/stat.rs");

fn section<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
    let start = source.find(start).expect("section start must exist");
    let end = source[start..]
        .find(end)
        .map(|offset| start + offset)
        .expect("section end must exist");
    &source[start..end]
}

#[test]
fn nice_is_owned_by_each_thread_not_process_data() {
    let thread = section(TASK, "pub struct Thread {", "impl Thread {");
    let process = section(TASK, "pub struct ProcessData {", "impl ProcessData {");
    assert!(thread.contains("nice: AtomicI32"));
    assert!(!process.contains("nice: AtomicI32"));
    assert!(TASK.contains("pub fn nice(&self) -> i32"));
    assert!(TASK.contains("pub fn set_nice(&self, nice: i32)"));
}

#[test]
fn priority_syscalls_target_tasks_and_clone_inherits_callers_nice() {
    assert!(SCHEDULE.contains("fn set_thread_scheduler_nice("));
    assert!(!SCHEDULE.contains("fn set_process_scheduler_nice("));
    assert!(SCHEDULE.contains("get_task(if who == 0"));
    assert!(CLONE.contains("thr.set_nice(child_nice);"));
}

#[test]
fn proc_task_stat_reports_the_signed_thread_nice_value() {
    assert!(STAT.contains("pub nice: i32"));
    assert!(STAT.contains("nice: thread.nice(),"));
}
