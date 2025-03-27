use std::sync::Arc;

use axerrno::AxError;
use axprocess::{Process, ProcessFilter};

#[test]
fn test_child() {
    let init = Process::new_init();

    let child = init.new_child();
    assert!(Arc::ptr_eq(&init, &child.parent().unwrap()));
}

#[test]
fn test_reap() {
    let init = Process::new_init();

    let child = init.new_child();
    let grandchild = child.new_child();

    child.exit();

    assert!(Arc::ptr_eq(&init, &grandchild.parent().unwrap()));
}

#[test]
fn test_exit() {
    let init = Process::new_init();

    assert_eq!(
        init.find_zombie_child(ProcessFilter::Any).err(),
        Some(AxError::NotFound)
    );

    let child = init.new_child();

    assert!(
        init.find_zombie_child(ProcessFilter::Any)
            .unwrap()
            .is_none()
    );

    child.exit();

    assert!(Arc::ptr_eq(
        &child,
        &init.find_zombie_child(ProcessFilter::Any).unwrap().unwrap()
    ));
    assert!(Arc::ptr_eq(
        &child,
        &init
            .find_zombie_child(ProcessFilter::Process(child.pid()))
            .unwrap()
            .unwrap()
    ));
    assert!(Arc::ptr_eq(
        &child,
        &init
            .find_zombie_child(ProcessFilter::ProcessGroup(child.group().pgid()))
            .unwrap()
            .unwrap()
    ));

    assert_eq!(
        init.find_zombie_child(ProcessFilter::Process(init.pid()))
            .err(),
        Some(AxError::NotFound)
    );

    child.free();

    assert_eq!(
        init.find_zombie_child(ProcessFilter::Any).err(),
        Some(AxError::NotFound)
    );
}
