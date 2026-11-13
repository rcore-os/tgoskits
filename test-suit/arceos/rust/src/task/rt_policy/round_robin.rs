pub(super) fn run() -> crate::TestResult {
    use std::{
        println,
        sync::{
            Arc,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
        thread, time,
        vec::Vec,
    };

    use ax_std::os::arceos::{
        api::task::{AxCpuMask, ax_set_current_affinity},
        task::sched::{CpuId, CpuSet, RtPriority, SchedulePolicy},
    };
    ax_set_current_affinity(AxCpuMask::one_shot(0)).unwrap();
    let ready = Arc::new(AtomicUsize::new(0));
    let go = Arc::new(AtomicBool::new(false));
    let seen = Arc::new(AtomicUsize::new(0));
    let failed = Arc::new(AtomicBool::new(false));
    let mut workers = Vec::new();
    for index in 0..2 {
        let ready = Arc::clone(&ready);
        let go = Arc::clone(&go);
        let seen = Arc::clone(&seen);
        let failed = Arc::clone(&failed);
        let mut affinity =
            CpuSet::empty(ax_std::os::arceos::task::sched::cpu_topology_len().unwrap());
        assert!(affinity.insert(CpuId::new(1)));
        workers.push(
            ax_std::os::arceos::thread::builder(std::format!("rr-gated-{index}"))
                .policy(SchedulePolicy::round_robin(RtPriority::new(50).unwrap()))
                .affinity(affinity)
                .spawn(move || {
                    assert!(ax_cpu::interrupt::irqs_enabled());
                    ready.fetch_or(1 << index, Ordering::Release);
                    // Voluntary yielding belongs only to test preparation. Both
                    // equal-priority RR tasks must exist before measuring rotation.
                    while !go.load(Ordering::Acquire) {
                        thread::yield_now();
                    }
                    seen.fetch_or(1 << index, Ordering::Release);
                    let started = time::Instant::now();
                    while seen.load(Ordering::Acquire) != 3 {
                        if started.elapsed() > time::Duration::from_secs(2) {
                            failed.store(true, Ordering::Release);
                            break;
                        }
                        core::hint::spin_loop();
                    }
                })
                .unwrap(),
        );
    }
    let started = time::Instant::now();
    while ready.load(Ordering::Acquire) != 3 {
        assert!(
            started.elapsed() < time::Duration::from_secs(3),
            "both RR workers must be published and ready"
        );
        thread::yield_now();
    }
    go.store(true, Ordering::Release);
    for worker in workers {
        worker.join().unwrap();
    }
    println!(
        "RR_GATED_READY={} SEEN={} FAILED={}",
        ready.load(Ordering::Acquire),
        seen.load(Ordering::Acquire),
        failed.load(Ordering::Acquire)
    );
    if failed.load(Ordering::Acquire) {
        return Err("ready RR peer did not run without voluntary yield");
    }
    Ok(())
}
