//! Delegation permission tests + pids.events coverage.

use super::mock::{ensure_init, test_guard};
use crate::{
    can_delegate_write_for_test, create_child, read_attr_at, root_id, set_delegated_to, write_attr,
    write_subtree_control,
};

#[test]
fn can_delegate_write_rules() {
    // Root may always write.
    assert!(can_delegate_write_for_test(0, None));
    assert!(can_delegate_write_for_test(0, Some(1000)));
    // Non-root may write only when delegated to exactly that uid.
    assert!(can_delegate_write_for_test(1000, Some(1000)));
    assert!(!can_delegate_write_for_test(1000, None));
    assert!(!can_delegate_write_for_test(1000, Some(1001)));
}

#[test]
fn subtree_control_requires_delegation_for_non_root() {
    let _g = test_guard();
    let mock = ensure_init();
    mock.reset();

    // Precondition: under cgroup v2, a child may enable `pids` in its own
    // subtree_control only if the parent (here root) already delegates it.
    // Make this explicit so the test is self-contained: the cgroup tree is a
    // process-global singleton and mock.reset() clears per-pid maps but NOT
    // the hierarchy's subtree_control, so relying on another test to have
    // enabled +pids on root makes this test order-dependent (flaky).
    write_subtree_control(root_id(), b"+pids").unwrap();

    let node = create_child(root_id(), "deleg_a").unwrap();

    // Unprivileged caller without delegation is rejected.
    mock.set_current_uid(1000);
    assert!(write_subtree_control(node, b"+pids").is_err());

    // Delegate the subtree to uid 1000: now the same caller may write.
    mock.set_current_uid(0);
    set_delegated_to(node, Some(1000)).unwrap();
    mock.set_current_uid(1000);
    assert!(write_subtree_control(node, b"+pids").is_ok());

    mock.set_current_uid(0);
}

#[test]
fn pids_events_counts_denied_forks() {
    let _g = test_guard();
    let mock = ensure_init();
    mock.reset();

    write_subtree_control(root_id(), b"+pids").unwrap();
    let node = create_child(root_id(), "deleg_pe").unwrap();
    write_attr(node, "pids.max", b"0").unwrap();

    // events starts at max 0.
    let mut buf = [0u8; 64];
    let n = read_attr_at(node, "pids.events", 0, &mut buf).unwrap();
    assert_eq!(core::str::from_utf8(&buf[..n]).unwrap(), "max 0\n");
}
