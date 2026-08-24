use std::{
    os::arceos::{
        api::task::{self as api, AxCpuMask, AxWaitQueueHandle, ax_set_current_affinity},
        modules::{
            ax_hal::percpu::this_cpu_id,
            ax_task::{CpuSet, set_current_thread_affinity, task_test_hooks},
        },
    },
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

const PROGRESS_TIMEOUT: Duration = Duration::from_secs(10);

fn wait_until(mut condition: impl FnMut() -> bool, message: &'static str) {
    let started = Instant::now();
    while !condition() {
        assert!(started.elapsed() < PROGRESS_TIMEOUT, "{message}");
        thread::yield_now();
    }
}

fn online_cpu_mask(cpu_count: usize) -> AxCpuMask {
    let mut mask = AxCpuMask::new();
    for cpu in 0..cpu_count {
        mask.set(cpu, true);
    }
    mask
}

pub fn run() -> crate::TestResult {
    static WAKEE_READY: AtomicBool = AtomicBool::new(false);
    static RELEASE_WAKEE: AtomicBool = AtomicBool::new(false);
    static WAKEE_WAIT: AxWaitQueueHandle = AxWaitQueueHandle::new();

    let cpu_count = thread::available_parallelism().unwrap().get();
    assert!(
        cpu_count >= 4,
        "task-fair-wake-idle-sibling requires SMP >= 4, got {cpu_count}"
    );
    WAKEE_READY.store(false, Ordering::Release);
    RELEASE_WAKEE.store(false, Ordering::Release);

    assert!(ax_set_current_affinity(AxCpuMask::one_shot(1)).is_ok());
    wait_until(
        || this_cpu_id() == 1,
        "test owner did not settle on the Fair waker CPU",
    );

    let wakee = thread::spawn(move || {
        assert!(ax_set_current_affinity(AxCpuMask::one_shot(0)).is_ok());
        assert_eq!(this_cpu_id(), 0);
        assert!(ax_set_current_affinity(online_cpu_mask(cpu_count)).is_ok());
        WAKEE_READY.store(true, Ordering::Release);
        api::ax_wait_queue_wait_until(&WAKEE_WAIT, || RELEASE_WAKEE.load(Ordering::Acquire), None);
    });
    let wakee_id = wakee.thread().id().as_u64().get();
    wait_until(
        || WAKEE_READY.load(Ordering::Acquire),
        "the Fair wakee did not publish readiness",
    );
    wait_until(
        || task_test_hooks::thread_is_blocked(wakee_id),
        "the Fair wakee did not block on its previous CPU",
    );
    wait_until(
        || task_test_hooks::cpu_nr_running(0).is_ok_and(|count| count == 0),
        "the wakee's previous CPU did not become idle",
    );

    let stop_occupier = Arc::new(AtomicBool::new(false));
    let occupier_ready = Arc::new(AtomicBool::new(false));
    let occupier = {
        let stop = Arc::clone(&stop_occupier);
        let ready = Arc::clone(&occupier_ready);
        thread::spawn(move || {
            assert!(ax_set_current_affinity(AxCpuMask::one_shot(0)).is_ok());
            assert_eq!(this_cpu_id(), 0);
            ready.store(true, Ordering::Release);
            while !stop.load(Ordering::Acquire) {
                thread::yield_now();
            }
        })
    };
    wait_until(
        || occupier_ready.load(Ordering::Acquire),
        "the previous-CPU occupier did not start",
    );
    wait_until(
        || task_test_hooks::cpu_nr_running(0).is_ok_and(|count| count == 1),
        "the previous CPU did not publish one Fair current",
    );
    wait_until(
        || {
            (2..cpu_count).all(|cpu| {
                task_test_hooks::cpu_nr_running(cpu as u32).is_ok_and(|count| count == 0)
            })
        },
        "the idle sibling CPUs did not become idle",
    );

    task_test_hooks::arm_wake_placement_probe(wakee_id);
    RELEASE_WAKEE.store(true, Ordering::Release);
    assert_eq!(api::ax_wait_queue_wake(&WAKEE_WAIT, 1), 1);
    let target = task_test_hooks::take_wake_placement_cpu()
        .expect("the Fair wake placement probe must complete");
    assert!(
        target >= 2 && target < cpu_count as u32,
        "Linux select_idle_sibling must choose an idle CPU instead of busy CPU{target}"
    );

    wakee.join().expect("the Fair wakee must exit normally");
    stop_occupier.store(true, Ordering::Release);
    occupier
        .join()
        .expect("the previous-CPU occupier must exit normally");
    set_current_thread_affinity(CpuSet::all(cpu_count))
        .expect("test owner must restore full affinity");
    Ok(())
}
