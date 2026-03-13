use std::sync::Arc;

use starry_process::init_proc;

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
fn exit() {
    let parent = init_proc();
    let child = parent.new_child();
    child.exit();
    assert!(child.is_zombie());
    assert!(parent.children().iter().any(|c| Arc::ptr_eq(c, &child)));
}

#[test]
#[should_panic]
fn free_not_zombie() {
    init_proc().new_child().free();
}

#[test]
fn free() {
    let parent = init_proc().new_child();
    let child = parent.new_child();
    child.exit();
    child.free();
    assert!(parent.children().is_empty());
}

#[test]
fn reap() {
    let init = init_proc();

    let parent = init.new_child();
    let child = parent.new_child();

    parent.exit();
    assert!(Arc::ptr_eq(&init, &child.parent().unwrap()));
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

    let last = child.exit_thread(101, 7);
    assert!(!last);
    assert_eq!(child.exit_code(), 7);

    child.group_exit();
    assert!(child.is_group_exited());

    let last2 = child.exit_thread(102, 3);
    assert!(last2);
    assert_eq!(child.exit_code(), 7);
}