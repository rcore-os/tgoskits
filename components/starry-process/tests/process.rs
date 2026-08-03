use std::{sync::Arc, time::Duration};

use starry_process::{ProcessCpuTime, ThreadExit, init_proc};

mod common;
use common::ProcessExt;

#[test]
fn child() {
    let parent = init_proc();
    let child = parent.new_child();
    assert!(Arc::ptr_eq(&parent, &child.parent().unwrap()));
    assert!(parent.children().iter().any(|c| Arc::ptr_eq(c, &child)));
}

#[test]
fn retire_removes_parent_and_group_links() {
    let parent = init_proc().new_child();
    let child = parent.new_child();
    child.retire();
    assert!(child.parent().is_none());
    assert!(parent.children().is_empty());
    assert!(
        !child
            .group()
            .processes()
            .iter()
            .any(|process| Arc::ptr_eq(process, &child))
    );
}

#[test]
fn reap() {
    let init = init_proc();

    let parent = init.new_child();
    let child = parent.new_child();

    parent.reparent_children_to(&init);
    assert!(Arc::ptr_eq(&init, &child.parent().unwrap()));
}

#[test]
fn child_subreaper_flag_is_process_local() {
    let parent = init_proc().new_child();
    assert!(!parent.is_child_subreaper());

    parent.set_child_subreaper(true);
    assert!(parent.is_child_subreaper());

    let child = parent.new_child();
    assert!(!child.is_child_subreaper());

    parent.set_child_subreaper(false);
    assert!(!parent.is_child_subreaper());
}

#[test]
fn reap_to_nearest_child_subreaper() {
    let subreaper = init_proc().new_child();
    subreaper.set_child_subreaper(true);

    let parent = subreaper.new_child();
    let child = parent.new_child();

    parent.reparent_children_to(&subreaper);

    assert!(Arc::ptr_eq(&subreaper, &child.parent().unwrap()));
    assert!(subreaper.children().iter().any(|c| Arc::ptr_eq(c, &child)));
    assert!(!parent.children().iter().any(|c| Arc::ptr_eq(c, &child)));
}

#[test]
fn reap_to_nearest_nested_child_subreaper() {
    let outer = init_proc().new_child();
    outer.set_child_subreaper(true);

    let inner = outer.new_child();
    inner.set_child_subreaper(true);

    let parent = inner.new_child();
    let child = parent.new_child();

    parent.reparent_children_to(&inner);

    assert!(Arc::ptr_eq(&inner, &child.parent().unwrap()));
}

#[test]
fn exiting_child_subreaper_reparents_to_next_subreaper() {
    let outer = init_proc().new_child();
    outer.set_child_subreaper(true);

    let inner = outer.new_child();
    inner.set_child_subreaper(true);

    let parent = inner.new_child();
    let child = parent.new_child();

    parent.reparent_children_to(&inner);
    assert!(Arc::ptr_eq(&inner, &child.parent().unwrap()));

    inner.reparent_children_to(&outer);
    assert!(Arc::ptr_eq(&outer, &child.parent().unwrap()));
}

#[test]
fn thread_exit() {
    let parent = init_proc();
    let child = parent.new_child();

    child.add_thread(101);
    child.add_thread(102);

    let mut threads = child.threads();
    threads.sort();
    assert_eq!(threads, vec![101, 102]);

    let first = child.exit_thread(
        101,
        7,
        ProcessCpuTime::new(Duration::from_millis(5), Duration::from_millis(7)),
    );
    assert_eq!(first, ThreadExit::Remaining);
    assert_eq!(child.exit_code(), 7);

    let mut snapshot = child.start_group_exit(9).unwrap();
    snapshot.sort();
    assert_eq!(snapshot, vec![102]);
    assert!(child.is_group_exited());

    let last = child.exit_thread(
        102,
        3,
        ProcessCpuTime::new(Duration::from_millis(2), Duration::from_millis(3)),
    );
    assert_eq!(
        last,
        ThreadExit::Last(ProcessCpuTime::new(
            Duration::from_millis(7),
            Duration::from_millis(10)
        ))
    );
    assert_eq!(child.exit_code(), 9);
    assert!(child.start_group_exit(11).is_none());
    assert_eq!(child.exit_code(), 9);
}

#[test]
fn repeated_thread_exit_does_not_report_last_twice() {
    let child = init_proc().new_child();
    child.add_thread(101);

    assert_eq!(
        child.exit_thread(
            101,
            7,
            ProcessCpuTime::new(Duration::from_millis(2), Duration::from_millis(3))
        ),
        ThreadExit::Last(ProcessCpuTime::new(
            Duration::from_millis(2),
            Duration::from_millis(3)
        ))
    );
    assert_eq!(
        child.exit_thread(
            101,
            9,
            ProcessCpuTime::new(Duration::from_secs(20), Duration::from_secs(30))
        ),
        ThreadExit::AlreadyExited,
        "an already-removed thread must not publish process exit again"
    );
    assert_eq!(
        child.exit_code(),
        7,
        "a duplicate exit must not overwrite the process exit status"
    );
}
