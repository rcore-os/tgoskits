//! Linux PID visibility must not depend on scheduler-object reclamation.

const TASK_OPS: &str = include_str!("../src/task/ops.rs");
const TASK: &str = include_str!("../src/task/mod.rs");
const EXECVE: &str = include_str!("../src/syscall/task/execve.rs");
const SCHEDULE: &str = include_str!("../src/syscall/task/schedule.rs");

#[test]
fn task_exit_detaches_the_live_tid_before_scheduler_resources_are_reclaimed() {
    let exit = function_body(TASK_OPS, "pub fn do_exit(");

    assert!(TASK_OPS.contains("pub fn remove_task_from_table(tid: Pid)"));
    assert!(exit.contains("remove_task_from_table(thr.tid())"));
}

#[test]
fn zombie_priority_is_snapshotted_in_linux_lifecycle_state() {
    let zombie = section(TASK_OPS, "struct ZombieEntry {", "static ZOMBIE_TABLE:");
    let getpriority = function_body(SCHEDULE, "pub fn sys_getpriority(");

    assert!(zombie.contains("nice: i32"));
    assert!(TASK_OPS.contains("pub fn get_zombie_nice(pid: Pid) -> Option<i32>"));
    assert!(getpriority.contains("get_zombie_nice(who)"));
    assert!(!getpriority.contains("is_zombie_pid(who) => Ok(20)"));
}

#[test]
fn leader_priority_is_published_before_thread_group_detach() {
    let exit = function_body(TASK_OPS, "pub fn do_exit(");
    let retire = exit
        .find("thr.proc_data.retire_leader_nice(thr.nice())")
        .expect("leader exit must publish its nice value");
    let detach = exit
        .find("process.exit_thread(thr.tid(), exit_code)")
        .expect("thread exit must detach from the thread group");

    assert!(retire < detach);
    assert!(TASK.contains("retired_leader_nice: SpinNoIrq<Option<i32>>"));
    assert!(EXECVE.contains("proc_data.clear_retired_leader_nice()"));
}

fn section<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
    let start = source
        .find(start)
        .unwrap_or_else(|| panic!("missing section start: {start}"));
    let end = source[start..]
        .find(end)
        .map(|offset| start + offset)
        .unwrap_or_else(|| panic!("missing section end: {end}"));
    &source[start..end]
}

fn function_body<'a>(source: &'a str, signature: &str) -> &'a str {
    let start = source
        .find(signature)
        .unwrap_or_else(|| panic!("missing function signature: {signature}"));
    let source = &source[start..];
    let brace = source
        .find('{')
        .unwrap_or_else(|| panic!("missing function body: {signature}"));
    let mut depth = 0usize;
    for (offset, byte) in source[brace..].bytes().enumerate() {
        match byte {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return &source[..brace + offset + 1];
                }
            }
            _ => {}
        }
    }
    panic!("unterminated function body: {signature}");
}
