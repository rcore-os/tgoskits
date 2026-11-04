//! Allocation-audit scopes: nesting on one CPU and overlap across CPUs.
use alloc::{boxed::Box, string::ToString, sync::Arc};
use core::{
    hint::{black_box, spin_loop},
    sync::atomic::{AtomicU32, AtomicUsize, Ordering},
};

use ax_alloc::alloc_audit::AllocAuditScope;
use ax_runtime::hal::time::monotonic_time_nanos;
use ax_std::os::arceos::task as scheduler;

/// Upper bound on each handshake, so a broken interleaving fails the test
/// instead of hanging it.
const HANDSHAKE_TIMEOUT_NANOS: u64 = 10_000_000_000;

fn allocate_once() {
    drop(black_box(Box::new(0u64)));
}

/// Spins until `stage` reaches `value`; `false` if the bound expired first.
fn wait_for(stage: &AtomicU32, value: u32) -> bool {
    let deadline = monotonic_time_nanos() + HANDSHAKE_TIMEOUT_NANOS;
    while stage.load(Ordering::Acquire) < value {
        if monotonic_time_nanos() > deadline {
            return false;
        }
        spin_loop();
    }
    true
}

fn single_cpu_set(cpu: u32) -> scheduler::sched::CpuSet {
    let mut set = scheduler::sched::CpuSet::empty(ax_runtime::hal::cpu_num());
    assert!(set.insert(scheduler::sched::CpuId::new(cpu)));
    set
}

#[axtest::axtest]
fn nested_scope_keeps_the_outer_count() {
    // An inner scope must not reset what the outer one has already counted.
    let outer = AllocAuditScope::enter();
    allocate_once();
    let inner = AllocAuditScope::enter();
    let inner_count = inner.finish();
    let outer_count = outer.finish();
    assert_eq!(inner_count, 0);
    assert_eq!(outer_count, 1);
}

#[axtest::axtest]
fn scope_ending_on_one_cpu_keeps_counting_on_another() {
    // Scope A (this task) and scope B (a peer on another CPU) overlap, and A
    // ends first: an allocation B makes afterwards must still be counted.
    let cpus = ax_runtime::hal::cpu_num();
    if cpus >= 2 {
        overlap_scopes_across_cpus(cpus);
    }
}

fn overlap_scopes_across_cpus(cpus: usize) {
    let current = scheduler::thread::current::current_thread_id()
        .expect("axtest must run in task context");
    let original_affinity = scheduler::thread::ThreadHandle::lookup(current)
        .and_then(|thread| thread.affinity())
        .expect("current affinity must be available");
    let this_cpu = u32::try_from(ax_runtime::hal::percpu::this_cpu_id())
        .expect("logical CPU ID must fit the scheduler ABI");
    let peer_cpu = (this_cpu + 1) % u32::try_from(cpus).expect("CPU count fits in a u32");
    scheduler::thread::current::set_current_thread_affinity(single_cpu_set(this_cpu))
        .expect("test task must stay on its CPU");

    // 1: A entered; 2: B entered; 3: A finished; 4: B finished.
    let stage = Arc::new(AtomicU32::new(0));
    let peer_count = Arc::new(AtomicUsize::new(usize::MAX));
    let peer_stage = Arc::clone(&stage);
    let peer_result = Arc::clone(&peer_count);
    let peer = ax_std::os::arceos::thread::builder("alloc-audit-peer".to_string())
        .stack_size(256 * 1024)
        .affinity(single_cpu_set(peer_cpu))
        .spawn(move || {
            if !wait_for(&peer_stage, 1) {
                return;
            }
            let scope = AllocAuditScope::enter();
            peer_stage.fetch_max(2, Ordering::AcqRel);
            let ordered = wait_for(&peer_stage, 3);
            if ordered {
                allocate_once();
            }
            let count = scope.finish();
            if ordered {
                peer_result.store(count, Ordering::Release);
            }
            peer_stage.fetch_max(4, Ordering::AcqRel);
        })
        .expect("failed to spawn the allocation-audit peer");

    let scope = AllocAuditScope::enter();
    stage.store(1, Ordering::Release);
    let overlapped = wait_for(&stage, 2);
    let count = scope.finish();
    stage.fetch_max(3, Ordering::AcqRel);

    peer.join().expect("allocation-audit peer must exit");
    scheduler::thread::current::set_current_thread_affinity(original_affinity)
        .expect("test task affinity must be restored");
    assert!(overlapped, "the peer scope did not start while this one was active");
    assert_eq!(count, 0);
    assert_eq!(
        peer_count.load(Ordering::Acquire),
        1,
        "an allocation after another CPU's scope ended went uncounted"
    );
}
