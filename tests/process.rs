use std::sync::Arc;

mod common;

#[test]
fn child() {
    let init = common::new_init();

    let child = common::fork(&init);
    assert!(Arc::ptr_eq(&init, &child.parent().unwrap()));
}

#[test]
fn exit() {
    let init = common::new_init();

    let child = common::fork(&init);

    child.exit();
    assert!(child.is_zombie());
    assert!(init.children().iter().any(|c| Arc::ptr_eq(c, &child)));
}

#[test]
#[should_panic]
fn free_not_zombie() {
    let init = common::new_init();
    let child = common::fork(&init);
    child.free();
}

#[test]
fn free() {
    let init = common::new_init();
    let child = common::fork(&init);
    child.exit();
    child.free();
    assert!(init.children().is_empty());
}

#[test]
fn reap() {
    let init = common::new_init();

    let child = common::fork(&init);
    let grandchild = common::fork(&child);

    child.exit();

    assert!(Arc::ptr_eq(&init, &grandchild.parent().unwrap()));
}
