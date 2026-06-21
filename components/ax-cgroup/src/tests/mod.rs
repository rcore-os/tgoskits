//! Host-side unit tests for `ax-cgroup`.
//!
//! These run under `cargo test -p ax-cgroup` on the host target. They use a
//! [`mock::MockProvider`] in place of the kernel provider. All test code is
//! gated behind `#[cfg(test)]` so it never affects the `no_std` build.

mod cpu_logic;
mod memory_charge;
mod mock;
mod parse;
mod roundtrip;

/// Test-only helper: register `pid` in node `id`'s process list, mirroring
/// what the membership layer does on a committed fork. Lets migration tests
/// exercise the move path without a full fork sequence.
fn add_proc_for_test(id: u64, pid: u32) {
    let node = crate::core::get_node(id).unwrap();
    let mut procs = node.procs.lock();
    if !procs.contains(&pid) {
        procs.push(pid);
    }
}
