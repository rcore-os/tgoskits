use std::sync::Arc;
use std::any::Any;

use starry_process::init_proc;

mod common;
use common::ProcessExt;

#[test]
fn basic() {
    let init = init_proc();
    let group = init.group();
    let session = group.session();

    assert_eq!(group.pgid(), init.pid());
    assert_eq!(session.sid(), init.pid());

    let process_groups = session.process_groups();
    assert!(process_groups.iter().any(|p| Arc::ptr_eq(p, &group)));
}

#[test]
fn create() {
    let parent = init_proc();
    let group = parent.group();
    let session = group.session();

    let child = parent.new_child();
    let (child_session, child_group) = child.create_session().unwrap();

    assert_eq!(child_group.pgid(), child.pid());
    assert_eq!(child_session.sid(), child.pid());
    assert!(Arc::ptr_eq(&child_group, &child.group()));
    assert!(Arc::ptr_eq(&child_session, &child_group.session()));
    assert_eq!(child_group.processes().len(), 1);
    assert_eq!(child_session.process_groups().len(), 1);

    assert!(group.processes().iter().all(|p| !Arc::ptr_eq(p, &child)));
    assert!(
        session
            .process_groups()
            .iter()
            .all(|g| !Arc::ptr_eq(g, &child_group))
    );
}

#[test]
fn create_leader() {
    assert!(init_proc().create_session().is_none());
}

#[test]
fn cleanup() {
    let child = init_proc().new_child();
    let session = {
        let (session, _) = child.create_session().unwrap();
        Arc::downgrade(&session)
    };

    assert!(session.upgrade().is_some());
    child.exit();
    child.free();
    drop(child);
    assert!(session.upgrade().is_none());
}

#[test]
fn create_group() {
    let parent = init_proc();
    let group = parent.group();
    let session = group.session();

    let child = parent.new_child();
    let child_group = child.create_group().unwrap();

    assert!(Arc::ptr_eq(&child_group.session(), &session));

    assert!(
        session
            .process_groups()
            .iter()
            .any(|p| Arc::ptr_eq(p, &child_group))
    );
}

#[test]
fn move_to_different_session() {
    let parent = init_proc().new_child();
    let child = parent.new_child();

    let (session, group) = parent.create_session().unwrap();

    assert!(!Arc::ptr_eq(&group, &child.group()));
    assert!(!Arc::ptr_eq(&session, &child.group().session()));

    assert!(!child.move_to_group(&group));
}

#[test]
fn cleanup_groups() {
    let child = init_proc().new_child();
    let (session, _) = child.create_session().unwrap();

    child.exit();
    child.free();
    drop(child);

    assert!(session.process_groups().is_empty());
}

#[test]
fn terminal_set_unset() {
    let init = init_proc();
    let session = init.group().session();

    let term: Arc<dyn Any + Send + Sync> = Arc::new(0u32);

    let ok = session.set_terminal_with(|| term.clone());
    assert!(ok);

    let ok2 = session.set_terminal_with(|| Arc::new(1u32));
    assert!(!ok2);

    let got = session.terminal().unwrap();
    assert!(Arc::ptr_eq(&got, &term));

    let other: Arc<dyn Any + Send + Sync> = Arc::new(2u32);
    assert!(!session.unset_terminal(&other));

    assert!(session.unset_terminal(&term));
    assert!(session.terminal().is_none());
}