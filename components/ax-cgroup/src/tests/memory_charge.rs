//! Memory hierarchical charge/uncharge tests via the public API + mock provider.

use super::mock::{ensure_init, test_guard};
use crate::{
    CgroupProvider, create_child, exit_process, lookup_child, migrate_process, read_attr_at,
    root_id, try_charge_memory, uncharge_memory, write_attr, write_subtree_control,
};

/// Read a node's `memory.current` byte counter.
fn mem_current(id: u64) -> u64 {
    let mut buf = [0u8; 64];
    let n = read_attr_at(id, "memory.current", 0, &mut buf).unwrap();
    core::str::from_utf8(&buf[..n])
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

/// Read a node's `memory.events` `max` counter.
fn mem_events_max(id: u64) -> u64 {
    let mut buf = [0u8; 128];
    let n = read_attr_at(id, "memory.events", 0, &mut buf).unwrap();
    core::str::from_utf8(&buf[..n])
        .unwrap()
        .lines()
        .find_map(|l| l.strip_prefix("max "))
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

/// Assign `pid` to cgroup `id` through the mock provider (as the kernel would).
fn place_pid(mock: &super::mock::MockProvider, pid: u32, id: u64) {
    let node = crate::core::get_node(id).unwrap();
    mock.set_cgroup(pid, node);
}

#[test]
fn memory_charge_uncharge_symmetry() {
    let _g = test_guard();
    let mock = ensure_init();
    mock.reset();
    write_subtree_control(root_id(), b"+memory").unwrap();
    let a = create_child(root_id(), "mc_sym").unwrap();
    place_pid(mock, 9001, a);

    assert_eq!(mem_current(a), 0);
    try_charge_memory(9001, 4096).unwrap();
    assert_eq!(mem_current(a), 4096);

    uncharge_memory(9001, 4096).unwrap();
    assert_eq!(mem_current(a), 0);
}

#[test]
fn memory_charge_over_limit_fails_and_bumps_events() {
    let _g = test_guard();
    let mock = ensure_init();
    mock.reset();
    write_subtree_control(root_id(), b"+memory").unwrap();
    let a = create_child(root_id(), "mc_limit").unwrap();
    write_attr(a, "memory.max", b"4096").unwrap();
    place_pid(mock, 9002, a);

    // First charge fits.
    try_charge_memory(9002, 4096).unwrap();
    assert_eq!(mem_current(a), 4096);

    // Over-limit charge is refused, current unchanged, events.max bumped.
    let before = mem_events_max(a);
    assert!(try_charge_memory(9002, 1).is_err());
    assert_eq!(mem_current(a), 4096);
    assert_eq!(mem_events_max(a), before + 1);
}

#[test]
fn memory_migrate_moves_charge() {
    let _g = test_guard();
    let mock = ensure_init();
    mock.reset();
    write_subtree_control(root_id(), b"+memory").unwrap();
    let src = create_child(root_id(), "mc_src").unwrap();
    let dst = create_child(root_id(), "mc_dst").unwrap();
    place_pid(mock, 9003, src);
    // Put the pid in src's procs list so migrate accepts it.
    write_attr(src, "memory.max", b"max").unwrap();
    crate::tests::add_proc_for_test(src, 9003);

    try_charge_memory(9003, 8192).unwrap();
    assert_eq!(mem_current(src), 8192);
    assert_eq!(mem_current(dst), 0);

    migrate_process(9003, dst).unwrap();
    assert_eq!(mem_current(src), 0);
    assert_eq!(mem_current(dst), 8192);

    // Exit releases the whole charge from the destination.
    exit_process(9003).unwrap();
    assert_eq!(mem_current(dst), 0);
}

#[test]
fn memory_hierarchical_charge_parent_and_child() {
    let _g = test_guard();
    let mock = ensure_init();
    mock.reset();
    write_subtree_control(root_id(), b"+memory").unwrap();
    let parent = create_child(root_id(), "mc_par").unwrap();
    write_subtree_control(parent, b"+memory").unwrap();
    let child = create_child(parent, "leaf").unwrap();
    place_pid(mock, 9004, child);

    try_charge_memory(9004, 2048).unwrap();
    // Both child and parent accrue the charge (hierarchical).
    assert_eq!(mem_current(child), 2048);
    assert_eq!(mem_current(parent), 2048);

    uncharge_memory(9004, 2048).unwrap();
    assert_eq!(mem_current(child), 0);
    assert_eq!(mem_current(parent), 0);

    let _ = lookup_child(parent, "leaf");
}
