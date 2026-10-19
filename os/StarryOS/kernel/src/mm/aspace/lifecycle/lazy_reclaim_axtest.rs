//! Real scheduler/MM reclamation after a scoped lazy-notification failure.

use alloc::sync::Arc;
use core::{hint::spin_loop, sync::atomic::{AtomicBool, Ordering}, time::Duration};

use ax_runtime::{
    task::{sched::{CpuId, CpuSet}, thread::current},
    thread::{TaskAddressSpace, UserContextOptions, builder, prepare_user_thread},
};

use super::{AddrSpace, MmHandle, MmState, Mutex, VirtAddr, run_reclaimer};

fn wait_until(timeout: Duration, mut ready: impl FnMut() -> bool) -> bool {
    let start = ax_hal::time::monotonic_time();
    while !ready() {
        if ax_hal::time::monotonic_time().saturating_sub(start) >= timeout {
            return false;
        }
        ax_std::thread::sleep(Duration::from_millis(1));
    }
    true
}

#[axtest::axtest]
fn lazy_mm_failed_notification_retries_on_the_production_reclaimer() {
    assert!(ax_hal::cpu_num() > 1, "lazy-MM retry requires two online CPUs");
    let original_affinity = current::current_thread_handle().unwrap().affinity().unwrap();
    let mut coordinator_affinity = CpuSet::empty(ax_hal::cpu_num());
    coordinator_affinity.insert(CpuId::new(0));
    current::set_current_thread_affinity(coordinator_affinity.clone()).unwrap();
    let mut target_affinity = CpuSet::empty(ax_hal::cpu_num());
    target_affinity.insert(CpuId::new(1));

    let aspace = Arc::new(Mutex::new(AddrSpace::new_empty(VirtAddr::from_usize(0x1000), 0x1000).unwrap()));
    let backing = Arc::downgrade(&aspace);
    let mm = MmHandle::from_arc(aspace).unwrap();
    let cpu_state = mm.inner.runtime_cpu_state.clone();
    cpu_state.fail_first_lazy_reclaim_for_test(1).unwrap();
    let user_mm = mm.scheduler_address_space().unwrap();
    let root = mm.installed().root();
    // SAFETY: scheduler_address_space retains this real MM and all its tables.
    // The entry remains in kernel mode; it only exercises user-context root
    // installation and the subsequent real switch into a lazy kernel context.
    let user = unsafe {
        prepare_user_thread(
            builder("lazy-reclaim-user".into()).affinity(target_affinity.clone()),
            move || {
                assert_eq!(ax_hal::cpu::mmu::read_user_page_table(), root);
                // Match Starry do_exit: release the task-scoped MM pin before
                // generic thread exit, retaining only the CPU's lazy lease.
                ax_runtime::thread::detach_current_address_space().unwrap();
            },
            UserContextOptions::new(user_mm),
        )
    }.unwrap().publish().unwrap();
    user.join().unwrap();
    assert!(wait_until(Duration::from_secs(3), || mm.inner.kernel_pins.load(Ordering::Acquire) == 0));
    assert_eq!(mm.active_cpu_mask(), 1 << 1, "target must retain the real lazy activation");

    let irq_held = Arc::new(AtomicBool::new(false));
    let release_irq = Arc::new(AtomicBool::new(false));
    let irq_timed_out = Arc::new(AtomicBool::new(false));
    let stop_reclaimer = Arc::new(AtomicBool::new(false));
    let target = {
        let irq_held = irq_held.clone();
        let release_irq = release_irq.clone();
        let irq_timed_out = irq_timed_out.clone();
        let cpu_state = cpu_state.clone();
        builder("lazy-reclaim-target".into()).affinity(target_affinity.clone()).prepare(move || {
            // This owner is already a kernel thread borrowing the preceding
            // real user MM. Freeze delivery until the coordinator observes a
            // retry. A local absolute deadline also releases IRQs on test failure.
            let preempt = crate::sync::PreemptGuard::new();
            let irq = ax_runtime::task::sync::IrqSaveGuard::new();
            let start = ax_hal::time::monotonic_time();
            irq_held.store(true, Ordering::Release);
            while !release_irq.load(Ordering::Acquire) {
                if ax_hal::time::monotonic_time().saturating_sub(start) >= Duration::from_millis(250) {
                    irq_timed_out.store(true, Ordering::Release);
                    break;
                }
                spin_loop();
            }
            drop(irq);
            // IRQs can now consume the real retry, but an exit or timer-driven
            // context switch must not reclaim the lazy MM ahead of that IPI.
            // Keep preemption disabled only for this bounded observation. A
            // missing retry still restores the guard and lets the task exit.
            let delivery_start = ax_hal::time::monotonic_time();
            while !cpu_state.lazy_reclaim_completed_for_test(1) {
                if ax_hal::time::monotonic_time().saturating_sub(delivery_start) >= Duration::from_millis(100) {
                    break;
                }
                spin_loop();
            }
            drop(preempt);
        }).unwrap()
    };
    let worker = {
        let stop = stop_reclaimer.clone();
        builder("lazy-reclaim-test-worker".into()).affinity(coordinator_affinity).prepare(move || {
            run_reclaimer(|| stop.load(Ordering::Acquire));
        }).unwrap()
    };
    // Publish every fallible prepared resource before closing the target IRQ
    // window. No assertion/panic below is allowed until the window is released.
    let worker = worker.publish().unwrap();
    let target = target.publish().unwrap();
    let entered = wait_until(Duration::from_secs(2), || irq_held.load(Ordering::Acquire));
    let permit = mm.release_user_ref();
    let first_failed = cpu_state.lazy_reclaim_attempts_for_test() >= 1;
    let retained = mm.active_cpu_mask() == 1 << 1 && mm.state() == MmState::Retiring;
    drop(permit);
    // Neither this thread nor the target calls request/retry/reap here. Only
    // the production worker implementation can create the second attempt.
    let retried = wait_until(Duration::from_millis(100), || cpu_state.lazy_reclaim_attempts_for_test() >= 2);
    release_irq.store(true, Ordering::Release);
    let target_result = target.join();
    let completed = wait_until(Duration::from_secs(3), || {
        cpu_state.lazy_reclaim_completed_for_test(1)
            && mm.active_cpu_mask() == 0
            && mm.inner.active_count.load(Ordering::Acquire) == 0
            && mm.state() == MmState::Freed
    });

    // On a deliberately broken implementation, retire the stale MM by a
    // different real user context only AFTER saving the failed measurement.
    // The target IRQ handshake is already released on every expected path.
    if !completed {
        let replacement = TaskAddressSpace::new(ax_hal::cpu::mmu::read_kernel_page_table(), ()).unwrap();
        // SAFETY: the replacement permanently owned kernel root is valid and
        // this cleanup entry cannot enter user mode or access user mappings.
        let cleanup = unsafe {
            prepare_user_thread(
                builder("lazy-reclaim-cleanup".into()).affinity(target_affinity),
                || {}, UserContextOptions::new(replacement),
            )
        }.unwrap().publish().unwrap();
        cleanup.join().unwrap();
    }
    drop(mm);
    let released = wait_until(Duration::from_secs(3), || backing.upgrade().is_none());
    stop_reclaimer.store(true, Ordering::Release);
    worker.join().unwrap();
    current::set_current_thread_affinity(original_affinity).unwrap();

    assert!(entered, "target never entered the bounded IRQ handshake");
    assert!(!irq_timed_out.load(Ordering::Acquire), "target IRQ handshake exceeded its deadline");
    target_result.unwrap();
    assert!(first_failed && retained, "the injected first failure must preserve the active MM");
    assert!(retried, "the reclaimer did not autonomously retry the failed notification");
    assert!(completed, "the real IPI consumer did not release root/TLB/leases and free the MM");
    assert!(released, "the retired MM backing did not reach its final release");
}
