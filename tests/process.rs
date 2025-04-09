use std::sync::Arc;

use axprocess::init_proc;

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
#[should_panic]
fn init_proc_exit() {
    init_proc().exit();
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
