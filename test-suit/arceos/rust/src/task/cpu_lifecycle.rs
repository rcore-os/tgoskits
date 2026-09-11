//! CPU offline requires a kernel without device workers pinned to the target.
//! Run separately from `all`, which starts block and network queue services.
//! Linux sched_cpu_wait_empty() follows smpboot/device hotplug teardown; this
//! probe tests the final quiescent-owner transaction, not that orchestration.
use std::{
    println,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

pub fn run() -> crate::TestResult {
    idle_cpu_reservation_round_trip();
    Ok(())
}

fn wait_for(condition: impl Fn() -> bool) {
    let started = std::time::Instant::now();
    while !condition() && started.elapsed() < Duration::from_secs(2) {
        thread::yield_now();
    }
    assert!(condition(), "CPU lifecycle event must complete");
}

fn wait_for_idle_cycle() -> bool {
    let started = std::time::Instant::now();
    loop {
        if let Some(result) = ax_runtime::thread::creation_probe::take_idle_cpu_round_trip_result()
        {
            return result;
        }
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "idle owner did not service CPU cycle"
        );
        thread::yield_now();
    }
}

fn idle_cpu_reservation_round_trip() {
    use ax_runtime::thread::creation_probe::request_idle_cpu_round_trip;
    use ax_std::os::arceos::task::{
        sched::{CpuId, CpuSet},
        thread::current,
    };
    assert!(
        ax_hal::cpu_num() >= 3,
        "CPU lifecycle requires at least three CPUs"
    );
    let original = current::current_thread_handle()
        .unwrap()
        .affinity()
        .unwrap();
    let mut coordinator = CpuSet::empty(ax_hal::cpu_num());
    coordinator.insert(CpuId::new(0));
    current::set_current_thread_affinity(coordinator).unwrap();
    let mut target = CpuSet::empty(ax_hal::cpu_num());
    target.insert(CpuId::new(1));
    offline_waits_for_owner_publication();
    for activate in [false, true] {
        let prepared = ax_runtime::thread::builder("hotplug-reservation".into())
            .affinity(target.clone())
            .prepare(move || assert!(activate, "cancelled entry ran"))
            .unwrap();
        let handle = prepared.thread_handle();
        let staged = prepared.stage().unwrap();
        request_idle_cpu_round_trip(1).unwrap();
        assert!(
            !wait_for_idle_cycle(),
            "staged publication must pin its target CPU"
        );
        if activate {
            staged.activate().join().unwrap();
        } else {
            drop(staged);
        }
        handle.wait().unwrap();
        wait_for(|| handle.execution_reclaimed());
        handle.join().unwrap();
        request_idle_cpu_round_trip(1).unwrap();
        assert!(
            wait_for_idle_cycle(),
            "released reservation must permit idle CPU cycle: {:?}",
            ax_task::runtime::cpu::idle_offline_rejection()
        );
        let resumed = ax_runtime::thread::builder("after-reonline".into())
            .affinity(target.clone())
            .spawn(|| {
                assert_eq!(ax_hal::percpu::this_cpu_id(), 1);
                thread::sleep(Duration::from_millis(2));
            })
            .unwrap();
        resumed.wait().unwrap();
        wait_for(|| resumed.execution_reclaimed());
        resumed.join().unwrap();
    }
    // The WFI-boundary probe verifies delivery, not transient quiescence.
    // Keep a real publication reservation so the offline result is determined
    // even if ordinary owner work arrives while idle leaves the wait boundary.
    let boundary = ax_runtime::thread::builder("idle-boundary-reservation".into())
        .affinity(target)
        .prepare(|| panic!("boundary reservation must never execute"))
        .unwrap();
    let boundary_handle = boundary.thread_handle();
    let boundary = boundary.stage().unwrap();
    ax_runtime::thread::creation_probe::request_idle_cpu_round_trip_at_wait(1).unwrap();
    assert!(
        !wait_for_idle_cycle(),
        "idle boundary publication must be serviced without bypassing its reservation"
    );
    drop(boundary);
    boundary_handle.wait().unwrap();
    wait_for(|| boundary_handle.execution_reclaimed());
    boundary_handle.join().unwrap();
    offline_does_not_lock_global_mm();
    current::set_current_thread_affinity(original).unwrap();
    println!("task_cpu_lifecycle: idle owner offline/online and reservation release OK");
}

fn offline_does_not_lock_global_mm() {
    use ax_runtime::thread::creation_probe::{
        request_idle_cpu_round_trip_after_work, take_idle_cpu_round_trip_result,
    };
    use ax_std::os::arceos::task::sched::{CpuId, CpuSet};
    let held = Arc::new(AtomicBool::new(false));
    let release = Arc::new(AtomicBool::new(false));
    let holder_ready = Arc::clone(&held);
    let holder_release = Arc::clone(&release);
    let mut affinity = CpuSet::empty(ax_hal::cpu_num());
    affinity.insert(CpuId::new(2));
    let holder = ax_runtime::thread::builder("global-mm-lock-holder".into())
        .affinity(affinity)
        .spawn(move || {
            let _mm = ax_mm::kernel_aspace().lock();
            holder_ready.store(true, Ordering::Release);
            while !holder_release.load(Ordering::Acquire) {
                core::hint::spin_loop();
            }
        })
        .unwrap();
    wait_for(|| held.load(Ordering::Acquire));
    request_idle_cpu_round_trip_after_work(1).unwrap();
    let started = std::time::Instant::now();
    let mut result = None;
    while result.is_none() && started.elapsed() < Duration::from_secs(2) {
        result = take_idle_cpu_round_trip_result();
        thread::yield_now();
    }
    // Unblock the old implementation before failing, so the failure never
    // strands a CPU spinning with IRQs off. Success requires an actual reply
    // while the unrelated address-space lock is still held.
    release.store(true, Ordering::Release);
    holder.join().unwrap();
    if result.is_none() {
        let _ = wait_for_idle_cycle();
    }
    assert_eq!(
        result,
        Some(true),
        "CPU offline must not acquire the global kernel MM lock"
    );
}

fn offline_waits_for_owner_publication() {
    use ax_runtime::thread::creation_probe::{
        request_idle_cpu_round_trip, take_idle_cpu_round_trip_result,
    };
    use ax_task::runtime::cpu::{
        IdleOfflineReader, IdleOfflineRejection, RuntimeCpuId, idle_offline_rejection,
        with_idle_offline_reader,
    };

    // CPU 0 retains the real publication guard until CPU 1 has attempted
    // admission. No command is republished and no offline result is retried.
    for reader in [
        IdleOfflineReader::OwnerDelivery,
        IdleOfflineReader::IdleBalance,
    ] {
        with_idle_offline_reader(RuntimeCpuId::new(1), reader, |remote| {
            request_idle_cpu_round_trip(1).unwrap();
            wait_for(|| !matches!(idle_offline_rejection(), IdleOfflineRejection::Unclassified));
            assert_eq!(
                remote.lifecycle_state(),
                ax_task::runtime::cpu::CpuLifecycleState::Inactive,
                "new placement must be closed while the existing publisher is retained"
            );
            assert_eq!(
                take_idle_cpu_round_trip_result(),
                None,
                "an in-flight owner publication must not complete the offline request"
            );
        })
        .unwrap();
        assert!(
            wait_for_idle_cycle(),
            "offline must complete after the publisher releases its lease"
        );
    }
}
