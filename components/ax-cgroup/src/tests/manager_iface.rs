//! systemd/docker manager interface coverage (work G):
//! cgroup.events (populated/frozen), cgroup.stat (nr_descendants),
//! memory.stat (controlled-format breakdown).

use super::mock::{ensure_init, test_guard};
use crate::{
    CgroupProvider, create_child, events_text, exit_process, read_attr_at, root_id, stat_text,
    write_attr, write_subtree_control,
};

fn add_pid(id: u64, pid: u32) {
    let node = crate::core::get_node(id).unwrap();
    let mut procs = node.procs.lock();
    if !procs.contains(&pid) {
        procs.push(pid);
    }
}

#[test]
fn cgroup_events_populated_follows_subtree() {
    let _g = test_guard();
    let _mock = ensure_init();

    let a = create_child(root_id(), "evt_a").unwrap();
    let b = create_child(a, "leaf").unwrap();

    // Both empty: populated 0.
    assert_eq!(events_text(a).unwrap(), "populated 0\nfrozen 0\n");
    assert_eq!(events_text(b).unwrap(), "populated 0\nfrozen 0\n");

    // A pid in the deep leaf populates every ancestor's subtree.
    add_pid(b, 7777);
    assert_eq!(events_text(b).unwrap(), "populated 1\nfrozen 0\n");
    assert_eq!(events_text(a).unwrap(), "populated 1\nfrozen 0\n");
}

#[test]
fn cgroup_stat_counts_only_descendants() {
    let _g = test_guard();
    let _mock = ensure_init();

    let a = create_child(root_id(), "stat_a").unwrap();
    assert_eq!(
        stat_text(a).unwrap(),
        "nr_descendants 0\nnr_dying_descendants 0\n"
    );

    let _b = create_child(a, "b1").unwrap();
    let _c = create_child(a, "c1").unwrap();
    let d = create_child(a, "d1").unwrap();
    let _e = create_child(d, "e1").unwrap();

    // a has 4 descendants total (b1, c1, d1, e1).
    assert_eq!(
        stat_text(a).unwrap(),
        "nr_descendants 4\nnr_dying_descendants 0\n"
    );
    // d has only e1.
    assert_eq!(
        stat_text(d).unwrap(),
        "nr_descendants 1\nnr_dying_descendants 0\n"
    );
}

#[test]
fn memory_stat_keys_present_with_honest_zeros() {
    let _g = test_guard();
    let _mock = ensure_init();

    write_subtree_control(root_id(), b"+memory").unwrap();
    let m = create_child(root_id(), "mstat").unwrap();

    let mut buf = [0u8; 512];
    let n = read_attr_at(m, "memory.stat", 0, &mut buf).unwrap();
    let s = core::str::from_utf8(&buf[..n]).unwrap();

    // Critical keys docker/runc expect must all appear, value 0 by default.
    for key in [
        "anon",
        "file",
        "kernel_stack",
        "shmem",
        "active_anon",
        "inactive_file",
    ] {
        assert!(s.lines().any(|l| l.starts_with(key)), "missing key {key}");
    }

    // memory.stat is read-only.
    assert!(write_attr(m, "memory.stat", b"x").is_err());
}

#[test]
fn cgroup_events_emits_inotify_on_populated_flip() {
    let _g = test_guard();
    let mock = ensure_init();
    mock.reset();

    // A leaf cgroup with one process: its whole ancestor chain is populated.
    let leaf = create_child(root_id(), "notif_leaf").unwrap();
    add_pid(leaf, 8888);
    let node = crate::core::get_node(leaf).unwrap();
    mock.set_cgroup(8888, node);

    // Last process exits -> populated flips 1 -> 0 for the leaf AND root,
    // so systemd's inotify watch on cgroup.events sees IN_MODIFY without polling.
    exit_process(8888).unwrap();

    let fired = mock.populated_notifications();
    assert!(
        fired.iter().any(|p| p == "/notif_leaf"),
        "expected populated notification for /notif_leaf, got {fired:?}"
    );
    assert!(
        fired.iter().any(|p| p == "/"),
        "expected populated notification for root, got {fired:?}"
    );
}
