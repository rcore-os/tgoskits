//! cpuset effective-mask tests: pure intersection + hierarchical recompute.

use super::mock::{ensure_init, test_guard};
use crate::{
    cpuset::CpusetState, create_child, read_attr_at, root_id, write_attr, write_subtree_control,
};

#[test]
fn effective_intersect_pure() {
    // Child keeps only CPUs the parent also allows.
    assert_eq!(CpusetState::effective_intersect(0b1111, 0b0110), 0b0110);
    assert_eq!(CpusetState::effective_intersect(0b0011, 0b0110), 0b0010);
    // Disjoint sets yield an empty effective mask.
    assert_eq!(CpusetState::effective_intersect(0b0001, 0b0010), 0);
    // All-ones parent leaves the child's request untouched.
    assert_eq!(CpusetState::effective_intersect(u64::MAX, 0b1010), 0b1010);
}

fn read_effective(id: u64) -> alloc::string::String {
    let mut buf = [0u8; 64];
    let n = read_attr_at(id, "cpuset.cpus.effective", 0, &mut buf).unwrap();
    let s = core::str::from_utf8(&buf[..n]).unwrap();
    s.trim_end().into()
}

#[test]
fn cpuset_effective_hierarchical_recompute() {
    let _g = test_guard();
    let _mock = ensure_init();

    write_subtree_control(root_id(), b"+cpuset").unwrap();
    let parent = create_child(root_id(), "cs_par").unwrap();
    write_attr(parent, "cpuset.cpus", b"0-3").unwrap();
    write_subtree_control(parent, b"+cpuset").unwrap();
    let child = create_child(parent, "cs_leaf").unwrap();

    // Child requests 2-5; effective is parent(0-3) ∩ own(2-5) = 2-3.
    write_attr(child, "cpuset.cpus", b"2-5").unwrap();
    assert_eq!(read_effective(child), "2-3");

    // Narrow the parent to 0-1: child effective recomputes to empty.
    write_attr(parent, "cpuset.cpus", b"0-1").unwrap();
    assert_eq!(read_effective(child), "");

    // Widen parent again to 0-3: child effective is 2-3 once more.
    write_attr(parent, "cpuset.cpus", b"0-3").unwrap();
    assert_eq!(read_effective(child), "2-3");
}
