use std::sync::Arc;

use axprocess::Process;

#[test]
fn basic() {
    let init = Process::new_init();
    let init_group = init.group();
    assert_eq!(init_group.pgid(), init.pid());

    let child = init.fork();
    assert!(Arc::ptr_eq(&init_group, &child.group()));

    let processes = init_group.processes();
    assert_eq!(processes.len(), 2);
    assert!(processes.iter().any(|p| Arc::ptr_eq(p, &init)));
    assert!(processes.iter().any(|p| Arc::ptr_eq(p, &child)));
}

#[test]
fn cleanup() {
    let init = Process::new_init();
    let group = Arc::downgrade(&init.group());

    assert!(group.upgrade().is_some());
    drop(init);
    assert!(group.upgrade().is_none());
}

#[test]
fn create() {
    let init = Process::new_init();

    let child = init.fork();
    let child_group = child.create_group().unwrap();

    assert!(Arc::ptr_eq(&child_group, &child.group()));
    assert_eq!(child_group.pgid(), child.pid());
    assert_eq!(child_group.processes().len(), 1);

    assert_eq!(init.group().processes().len(), 1);
}

#[test]
fn create_leader() {
    let init = Process::new_init();
    let init_group = init.group();

    assert!(init.create_group().is_none());
    assert!(Arc::ptr_eq(&init_group, &init.group()));
}

#[test]
fn inherit() {
    let init = Process::new_init();

    let child = init.fork();
    let child_group = child.create_group().unwrap();

    let grandchild = child.fork();
    assert!(Arc::ptr_eq(&child_group, &grandchild.group()));
    assert_eq!(child_group.processes().len(), 2);
}

#[test]
fn move_to() {
    let init = Process::new_init();
    let init_group = init.group();

    let child1 = init.fork();
    let child1_group = child1.create_group().unwrap();

    assert!(child1.move_to_group(&child1.group()));
    assert!(Arc::ptr_eq(&child1_group, &child1.group()));
    assert_eq!(child1_group.processes().len(), 1);

    let child2 = init.fork();
    let child2_group = child2.create_group().unwrap();

    assert!(child2.move_to_group(&child1_group));
    assert!(Arc::ptr_eq(&child1_group, &child2.group()));

    assert_eq!(child1_group.processes().len(), 2);
    assert_eq!(child2_group.processes().len(), 0);
    assert_eq!(init_group.processes().len(), 1);
}

#[test]
fn move_cleanup() {
    let init = Process::new_init();
    let init_group = init.group();

    let child = init.fork();
    let child_group = Arc::downgrade(&child.create_group().unwrap());

    assert!(child_group.upgrade().is_some());
    assert!(child.move_to_group(&init_group));
    assert!(child_group.upgrade().is_none());
}

#[test]
fn move_back() {
    let init = Process::new_init();
    let init_group = init.group();

    let child = init.fork();
    let child_group = child.create_group().unwrap();

    assert!(child.move_to_group(&init_group));
    assert!(child.move_to_group(&child_group));

    assert!(Arc::ptr_eq(&child_group, &child.group()));
    assert_eq!(child_group.processes().len(), 1);
    assert_eq!(init_group.processes().len(), 1);
}
