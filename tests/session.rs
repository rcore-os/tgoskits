use std::sync::Arc;

use axprocess::Process;

#[test]
fn basic() {
    let init = Process::new_init();
    let init_group = init.group();
    let init_session = init_group.session();

    assert_eq!(init_group.pgid(), init.pid());
    assert_eq!(init_session.sid(), init.pid());

    let process_groups = init_session.process_groups();
    assert_eq!(process_groups.len(), 1);
    assert!(process_groups.iter().any(|p| Arc::ptr_eq(p, &init_group)));
}

#[test]
fn create() {
    let init = Process::new_init();
    let init_group = init.group();
    let init_session = init_group.session();

    let child = init.fork();
    let (child_session, child_group) = child.create_session().unwrap();

    assert_eq!(child_group.pgid(), child.pid());
    assert_eq!(child_session.sid(), child.pid());
    assert!(Arc::ptr_eq(&child_group, &child.group()));
    assert!(Arc::ptr_eq(&child_session, &child_group.session()));

    assert_eq!(init_group.processes().len(), 1);
    assert_eq!(init_session.process_groups().len(), 1);

    assert_eq!(child_group.processes().len(), 1);
    assert_eq!(child_session.process_groups().len(), 1);
}

#[test]
fn create_group() {
    let init = Process::new_init();
    let init_group = init.group();
    let init_session = init_group.session();

    let child = init.fork();
    let child_group = child.create_group().unwrap();

    assert!(Arc::ptr_eq(&child_group.session(), &init_session));

    let process_groups = init_session.process_groups();
    assert_eq!(process_groups.len(), 2);
    assert!(process_groups.iter().any(|p| Arc::ptr_eq(p, &init_group)));
    assert!(process_groups.iter().any(|p| Arc::ptr_eq(p, &child_group)));
}

#[test]
fn move_to_different_session() {
    let init = Process::new_init();

    let child = init.fork();
    let grandchild = child.fork();

    let (child_session, child_group) = child.create_session().unwrap();

    assert!(!Arc::ptr_eq(&child_group, &grandchild.group()));
    assert!(!Arc::ptr_eq(&child_session, &grandchild.group().session()));

    assert!(!grandchild.move_to_group(&child_group));
}
