use std::sync::Arc;

use axprocess::Process;

#[test]
fn child() {
    let init = Process::new_init();

    let child = init.fork();
    assert!(Arc::ptr_eq(&init, &child.parent().unwrap()));
}

#[test]
fn exit() {
    let init = Process::new_init();

    let child = init.fork();

    child.exit();
    assert!(child.is_zombie());
    assert!(init.children().iter().any(|c| Arc::ptr_eq(c, &child)));
}

#[test]
#[should_panic]
fn free_not_zombie() {
    let init = Process::new_init();
    let child = init.fork();
    child.free();
}

#[test]
fn free() {
    let init = Process::new_init();
    let child = init.fork();
    child.exit();
    child.free();
    assert!(init.children().is_empty());
}

#[test]
fn reap() {
    let init = Process::new_init();

    let child = init.fork();
    let grandchild = child.fork();

    child.exit();

    assert!(Arc::ptr_eq(&init, &grandchild.parent().unwrap()));
}
