use std::sync::Arc;

use axprocess::init_proc;

mod common;
use common::ProcessExt;

#[test]
fn basic() {
    let init = init_proc();
    let group = init.group();
    assert_eq!(group.pgid(), init.pid());

    let child = init.new_child();
    assert!(Arc::ptr_eq(&group, &child.group()));

    let processes = group.processes();
    assert!(processes.iter().any(|p| Arc::ptr_eq(p, &init)));
    assert!(processes.iter().any(|p| Arc::ptr_eq(p, &child)));
}

#[test]
fn create() {
    let parent = init_proc();

    let child = parent.new_child();
    let child_group = child.create_group().unwrap();

    assert!(Arc::ptr_eq(&child_group, &child.group()));
    assert_eq!(child_group.pgid(), child.pid());

    let child_group_processes = child_group.processes();
    assert_eq!(child_group_processes.len(), 1);
    assert!(Arc::ptr_eq(&child_group_processes[0], &child));

    assert!(
        parent
            .group()
            .processes()
            .iter()
            .all(|p| !Arc::ptr_eq(p, &child))
    );
}

#[test]
fn create_leader() {
    let init = init_proc();
    let group = init.group();

    assert!(init.create_group().is_none());
    assert!(Arc::ptr_eq(&group, &init.group()));
}

#[test]
fn cleanup() {
    let child = init_proc().new_child();

    let group = Arc::downgrade(&child.create_group().unwrap());
    assert!(group.upgrade().is_some());

    child.exit();
    child.free();
    drop(child);
    assert!(group.upgrade().is_none());
}

#[test]
fn inherit() {
    let parent = init_proc().new_child();
    let group = parent.create_group().unwrap();

    let child = parent.new_child();
    assert!(Arc::ptr_eq(&group, &child.group()));
    assert_eq!(group.processes().len(), 2);
}

#[test]
fn move_to() {
    let parent = init_proc();

    let child1 = parent.new_child();
    let child1_group = child1.create_group().unwrap();

    assert!(child1.move_to_group(&child1.group()));
    assert!(Arc::ptr_eq(&child1_group, &child1.group()));
    assert_eq!(child1_group.processes().len(), 1);

    let child2 = parent.new_child();
    let child2_group = child2.create_group().unwrap();

    assert!(child2.move_to_group(&child1_group));
    assert!(Arc::ptr_eq(&child1_group, &child2.group()));

    let child1_group_processes = child1_group.processes();
    assert_eq!(child1_group_processes.len(), 2);
    assert!(Arc::ptr_eq(&child1_group_processes[0], &child1));
    assert!(Arc::ptr_eq(&child1_group_processes[1], &child2));

    assert_eq!(child2_group.processes().len(), 0);
}

#[test]
fn move_cleanup() {
    let parent = init_proc();
    let group = parent.group();

    let child = parent.new_child();
    let child_group = Arc::downgrade(&child.create_group().unwrap());

    assert!(child_group.upgrade().is_some());
    assert!(child.move_to_group(&group));
    assert!(child_group.upgrade().is_none());
}

#[test]
fn move_back() {
    let parent = init_proc();
    let group = parent.group();

    let child = parent.new_child();
    let child_group = child.create_group().unwrap();

    assert!(child.move_to_group(&group));
    assert!(child.move_to_group(&child_group));

    assert!(Arc::ptr_eq(&child_group, &child.group()));

    assert!(group.processes().iter().any(|p| !Arc::ptr_eq(p, &child)));
}

#[test]
fn cleanup_processes() {
    let parent = init_proc().new_child();
    let group = parent.create_group().unwrap();

    parent.exit();
    parent.free();
    drop(parent);

    assert!(group.processes().is_empty());
}
