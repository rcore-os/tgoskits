//! CPU offline requires a kernel without device workers pinned to the target.
//! Run separately from `all`, which starts block and network queue services.
//! Linux sched_cpu_wait_empty() follows smpboot/device hotplug teardown; this
//! probe tests the final quiescent-owner transaction, not that orchestration.
//! The reaper is deliberately kept on CPU 0: offlining a target with runnable
//! background services remains outside this test's coverage.
use std::{
    println,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::Duration,
};

pub fn run() -> crate::TestResult {
    assert!(
        ax_hal::cpu_num() >= 3,
        "CPU lifecycle requires at least three CPUs"
    );
    let _reaper_affinity = isolate_reaper_from_offline_targets();
    idle_polling_ends_before_task_execution();
    idle_cpu_reservation_round_trip();
    Ok(())
}

struct ReaperAffinity {
    thread: ax_task::thread::ThreadHandle,
    original: ax_task::sched::CpuSet,
}

impl Drop for ReaperAffinity {
    fn drop(&mut self) {
        self.thread
            .set_affinity_and_wait(self.original.clone())
            .unwrap();
    }
}

struct ReaperMigrationProbe {
    observed_cpu: AtomicUsize,
    release: AtomicBool,
    timed_out: AtomicBool,
}

struct ReaperMigrationCallback(Arc<ReaperMigrationProbe>);

impl Drop for ReaperMigrationCallback {
    fn drop(&mut self) {
        let started = std::time::Instant::now();
        // Cancelling an unpublished thread drops this capture on the real
        // reaper. Keep the same callback alive across its affinity migration.
        while !self.0.release.load(Ordering::Acquire) {
            self.0
                .observed_cpu
                .store(ax_hal::percpu::this_cpu_id(), Ordering::Release);
            if started.elapsed() >= Duration::from_secs(3) {
                // Report through the probe instead of panicking inside the
                // production reaper while it owns execution reclamation.
                self.0.timed_out.store(true, Ordering::Release);
                return;
            }
            thread::yield_now();
        }
    }
}

fn isolate_reaper_from_offline_targets() -> ReaperAffinity {
    use ax_task::{
        runtime::service::deferred_task_work_service_thread,
        sched::{CpuId, CpuSet},
        thread::ThreadHandle,
    };

    let reaper = ThreadHandle::lookup(deferred_task_work_service_thread().unwrap()).unwrap();
    let saved = ReaperAffinity {
        original: reaper.affinity().unwrap(),
        thread: reaper,
    };
    let mut target = CpuSet::empty(ax_hal::cpu_num());
    target.insert(CpuId::new(1));
    saved.thread.set_affinity_and_wait(target).unwrap();
    let probe = Arc::new(ReaperMigrationProbe {
        observed_cpu: AtomicUsize::new(usize::MAX),
        // Failed preparation may drop the capture on this caller. Arm its
        // rendezvous only after the unpublished thread was created successfully.
        release: AtomicBool::new(true),
        timed_out: AtomicBool::new(false),
    });
    let callback = ReaperMigrationCallback(probe.clone());
    let prepared = ax_runtime::thread::builder("reaper-offline-isolation".into())
        .prepare(move || drop(callback))
        .unwrap();
    let cancelled = prepared.thread_handle();
    probe.release.store(false, Ordering::Release);
    drop(prepared);
    wait_for_reaper_cpu(&probe, 1);

    // Execution reclamation and join do not park the background service. Linux
    // CPU hotplug moves workers before sched_cpu_wait_empty(); this final-owner
    // probe likewise needs a persistent exclusion, not a transient idle sample.
    let mut housekeeping = CpuSet::empty(ax_hal::cpu_num());
    housekeeping.insert(CpuId::new(0));
    saved.thread.set_affinity_and_wait(housekeeping).unwrap();
    wait_for_reaper_cpu(&probe, 0);
    probe.release.store(true, Ordering::Release);
    cancelled.join().unwrap();
    assert!(
        !probe.timed_out.load(Ordering::Acquire),
        "reaper migration callback timed out before release"
    );
    saved
}

fn wait_for_reaper_cpu(probe: &ReaperMigrationProbe, cpu: usize) {
    let started = std::time::Instant::now();
    while probe.observed_cpu.load(Ordering::Acquire) != cpu
        && !probe.timed_out.load(Ordering::Acquire)
    {
        assert!(
            started.elapsed() < Duration::from_secs(4),
            "reaper migration callback did not run on CPU {cpu}"
        );
        thread::yield_now();
    }
    assert!(
        !probe.timed_out.load(Ordering::Acquire),
        "reaper migration callback timed out before reaching CPU {cpu}"
    );
}

fn idle_polling_ends_before_task_execution() {
    use ax_task::{
        runtime::cpu::{
            IdlePollingWakeProbe, RuntimeCpuId, current_cpu_is_idle_polling, notify_idle_cpu_probe,
        },
        sched::{CpuId, CpuSet},
        sync::WaitQueue,
        thread::{ThreadState, current},
    };

    let original = current::current_thread_handle()
        .unwrap()
        .affinity()
        .unwrap();
    let mut coordinator = CpuSet::empty(ax_hal::cpu_num());
    coordinator.insert(CpuId::new(0));
    current::set_current_thread_affinity(coordinator).unwrap();
    let mut target = CpuSet::empty(ax_hal::cpu_num());
    target.insert(CpuId::new(1));
    let wakee = ax_runtime::thread::builder("idle-polling-wake".into())
        .affinity(target)
        .spawn(|| {
            WaitQueue::new().wait();
            assert!(
                !current_cpu_is_idle_polling().unwrap(),
                "idle polling must be withdrawn before a normal task runs"
            );
        })
        .unwrap();
    wait_for(|| wakee.state() == ThreadState::Blocked);
    {
        let _irq = ax_std::os::arceos::guard::PreemptIrqSaveGuard::new();
        assert!(
            matches!(
                IdlePollingWakeProbe::arm(CpuId::new(1), wakee.wake_handle()),
                Err(ax_task::thread::TaskError::UnsafeContext)
            ),
            "idle wake probe must reject atomic context before registration"
        );
    }
    let probe = IdlePollingWakeProbe::arm(CpuId::new(1), wakee.wake_handle()).unwrap();
    notify_idle_cpu_probe(RuntimeCpuId::new(1)).unwrap();
    wakee.join().unwrap();
    drop(probe);
    current::set_current_thread_affinity(original).unwrap();
}

fn wait_for(condition: impl Fn() -> bool) {
    let started = std::time::Instant::now();
    while !condition() {
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "CPU lifecycle event must complete"
        );
        thread::yield_now();
    }
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
    let original = current::current_thread_handle()
        .unwrap()
        .affinity()
        .unwrap();
    let mut coordinator = CpuSet::empty(ax_hal::cpu_num());
    coordinator.insert(CpuId::new(0));
    current::set_current_thread_affinity(coordinator).unwrap();
    let mut target = CpuSet::empty(ax_hal::cpu_num());
    target.insert(CpuId::new(1));
    offline_waits_for_owner_publication(&target);
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
        "CPU offline must drain owner work while the global kernel MM lock is held: {:?}",
        ax_task::runtime::cpu::idle_offline_rejection()
    );
}

fn offline_waits_for_owner_publication(target: &ax_task::sched::CpuSet) {
    use ax_runtime::thread::creation_probe::{
        request_idle_cpu_round_trip, take_idle_cpu_round_trip_result,
    };
    use ax_task::{
        runtime::cpu::{
            CpuLifecycleState, IdleOfflineReader, RuntimeCpuId, with_idle_offline_reader,
        },
        time::{
            MonotonicDeadline,
            timer::{KernelTimerCancelOutcome, cancel_kernel_timer, register_kernel_timer},
        },
    };

    // This is the scheduler's final offline transaction, not Linux's full
    // hotplug orchestration: it cannot migrate an outstanding kernel timer.
    // Establish a known veto instead of assuming reader release alone makes
    // every task and timer on the target quiescent.
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    let registrar = ax_runtime::thread::builder("offline-timer-owner".into())
        .affinity(target.clone())
        .spawn(move || {
            let deadline = MonotonicDeadline::from_duration(
                ax_std::os::arceos::api::time::ax_monotonic_time() + Duration::from_secs(60),
            );
            let timer = register_kernel_timer(
                deadline,
                Box::new(|_| panic!("offline test must cancel its timer")),
            )
            .unwrap();
            sender.send(timer).unwrap();
        })
        .unwrap();
    registrar.wait().unwrap();
    wait_for(|| registrar.execution_reclaimed());
    registrar.join().unwrap();
    let timer = receiver.recv().unwrap();

    // Retain the reader until placement closes. Inactive is stable while
    // this reader exists; the last diagnostic rejection is only a snapshot.
    // Like Linux cpu-on-off-test.sh, check the resulting state after each
    // operation, then remove the veto before expecting a successful cycle.
    for reader in [
        IdleOfflineReader::OwnerDelivery,
        IdleOfflineReader::IdleBalance,
    ] {
        with_idle_offline_reader(RuntimeCpuId::new(1), reader, |remote| {
            request_idle_cpu_round_trip(1).unwrap();
            wait_for(|| remote.lifecycle_state() == CpuLifecycleState::Inactive);
            assert_eq!(
                remote.lifecycle_state(),
                CpuLifecycleState::Inactive,
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
            !wait_for_idle_cycle(),
            "reader release must not bypass an outstanding kernel timer"
        );
        // A rejected offline operation must restore placement admission.
        with_idle_offline_reader(
            RuntimeCpuId::new(1),
            IdleOfflineReader::OwnerDelivery,
            |remote| {
                assert_eq!(remote.lifecycle_state(), CpuLifecycleState::Online);
            },
        )
        .unwrap();
    }
    assert_eq!(
        cancel_kernel_timer(timer),
        Ok(KernelTimerCancelOutcome::Cancelled)
    );
    request_idle_cpu_round_trip(1).unwrap();
    assert!(
        wait_for_idle_cycle(),
        "removing the timer veto must permit offline after both readers were released"
    );
}
