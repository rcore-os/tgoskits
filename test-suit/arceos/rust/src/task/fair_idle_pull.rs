use std::{
    os::arceos::{
        api::task::{AxCpuMask, ax_set_current_affinity},
        modules::{
            ax_hal::percpu::this_cpu_id,
            ax_task::{
                CpuId, CpuSet, KernelThreadHandle, ThreadBuilder, set_current_thread_affinity,
                task_test_hooks,
            },
        },
    },
    string::String,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
    vec::Vec,
};

const REMOTE_OCCUPIERS: usize = 1;
const SOURCE_WORKERS: usize = 4;
const PROGRESS_TIMEOUT: Duration = Duration::from_secs(10);

struct CooperativeWorkers {
    stop: Arc<AtomicBool>,
    ready: Arc<AtomicUsize>,
    worker_count: usize,
    handles: Vec<KernelThreadHandle>,
}

impl CooperativeWorkers {
    fn spawn_pinned(
        cpu: usize,
        cpu_count: usize,
        worker_count: usize,
        expand_affinity: bool,
        observed_cpus: Arc<AtomicUsize>,
    ) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let ready = Arc::new(AtomicUsize::new(0));
        let mut affinity = CpuSet::empty(cpu_count);
        assert!(affinity.insert(CpuId::new(cpu as u32)));
        let mut handles = Vec::with_capacity(worker_count);
        for _ in 0..worker_count {
            let stop = Arc::clone(&stop);
            let ready = Arc::clone(&ready);
            let observed_cpus = Arc::clone(&observed_cpus);
            handles.push(
                ThreadBuilder::new(String::from("fair-idle-pull-worker"))
                    .affinity(affinity.clone())
                    .spawn(move || {
                        assert_eq!(this_cpu_id(), cpu);
                        if expand_affinity {
                            set_current_thread_affinity(CpuSet::all(cpu_count))
                                .expect("source worker must widen its affinity in place");
                        }
                        ready.fetch_add(1, Ordering::Release);
                        while !stop.load(Ordering::Acquire) {
                            let running_cpu = this_cpu_id();
                            observed_cpus.fetch_or(1usize << running_cpu, Ordering::Relaxed);
                            thread::yield_now();
                        }
                    })
                    .expect("cooperative Fair worker must spawn"),
            );
        }
        Self {
            stop,
            ready,
            worker_count,
            handles,
        }
    }

    fn wait_ready(&self) {
        wait_until(
            || self.ready.load(Ordering::Acquire) == self.worker_count,
            "cooperative Fair workers did not become runnable",
        );
    }

    fn stop_and_join(mut self) {
        self.stop.store(true, Ordering::Release);
        for handle in self.handles.drain(..) {
            handle.join().expect("cooperative Fair worker must exit");
        }
    }
}

fn wait_until(mut condition: impl FnMut() -> bool, message: &'static str) {
    let started = Instant::now();
    while !condition() {
        assert!(started.elapsed() < PROGRESS_TIMEOUT, "{message}");
        thread::yield_now();
    }
}

fn wait_for_idle_pull(observed_cpus: &AtomicUsize) {
    let started = Instant::now();
    let mut observed_idle = false;
    loop {
        if task_test_hooks::fair_idle_pull_migration_target() == Some(1) {
            return;
        }
        let nr_running = task_test_hooks::cpu_nr_running(1)
            .expect("idle-pull target load observation must read the real scheduler");
        observed_idle |= nr_running == 0;
        assert!(
            started.elapsed() < PROGRESS_TIMEOUT,
            "CPU1 idle entry did not request a Fair migration from CPU0: \
             observed_idle={observed_idle}, nr_running={nr_running}, source={:?}, target={:?}, \
             observed_cpus={:#x}",
            task_test_hooks::fair_idle_pull_source(1),
            task_test_hooks::fair_idle_pull_migration_target(),
            observed_cpus.load(Ordering::Acquire),
        );
        thread::yield_now();
    }
}

pub fn run() -> crate::TestResult {
    let cpu_count = thread::available_parallelism().unwrap().get();
    assert!(
        cpu_count >= 2,
        "task-fair-idle-pull requires SMP >= 2, got {cpu_count}"
    );
    assert!(ax_set_current_affinity(AxCpuMask::one_shot(0)).is_ok());
    wait_until(
        || this_cpu_id() == 0,
        "test owner did not settle on the Fair source CPU",
    );
    task_test_hooks::set_current_fair_periodic_balance(false)
        .expect("the test owner must isolate new-idle balance from periodic source push");

    let observed_cpus = Arc::new(AtomicUsize::new(0));
    let occupier_cpus = Arc::new(AtomicUsize::new(0));
    let mut remote_occupiers = Vec::with_capacity(cpu_count - 1);
    for cpu in 1..cpu_count {
        let occupiers = CooperativeWorkers::spawn_pinned(
            cpu,
            cpu_count,
            REMOTE_OCCUPIERS,
            false,
            Arc::clone(&occupier_cpus),
        );
        occupiers.wait_ready();
        remote_occupiers.push(occupiers);
    }

    let source_workers = CooperativeWorkers::spawn_pinned(
        0,
        cpu_count,
        SOURCE_WORKERS,
        true,
        Arc::clone(&observed_cpus),
    );
    source_workers.wait_ready();
    assert_eq!(
        task_test_hooks::fair_idle_pull_source(1)
            .expect("idle-pull source observation must read the real scheduler"),
        Some(0),
        "Linux newidle balance must discover a Fair backlog before periodic balance"
    );

    task_test_hooks::reset_fair_idle_pull_migration();
    task_test_hooks::fail_next_fair_idle_pull_transfer(1);
    remote_occupiers.remove(0).stop_and_join();
    wait_until(
        task_test_hooks::fair_idle_pull_failure_completed,
        "the Fair idle-pull source did not handle the injected transfer failure",
    );
    assert_eq!(
        task_test_hooks::fair_idle_pull_retry_kicks(),
        0,
        "Linux newidle balance must end a failed pass instead of kicking the idle CPU to retry"
    );
    let idle_entries = task_test_hooks::fair_idle_pull_failure_idle_entries();
    let migration_target = task_test_hooks::fair_idle_pull_migration_target();
    assert!(
        idle_entries > 1 || migration_target.is_none(),
        "the failed newidle pass migrated a second candidate without a new idle event: \
         idle_entries={idle_entries}, target={migration_target:?}"
    );

    if migration_target.is_none() {
        let target_occupiers = CooperativeWorkers::spawn_pinned(
            1,
            cpu_count,
            REMOTE_OCCUPIERS,
            false,
            Arc::clone(&occupier_cpus),
        );
        target_occupiers.wait_ready();
        target_occupiers.stop_and_join();
        wait_for_idle_pull(&observed_cpus);
    }
    wait_until(
        || observed_cpus.load(Ordering::Acquire) & (1usize << 1) != 0,
        "the Fair idle-pull carrier did not execute on CPU1",
    );
    task_test_hooks::reset_fair_idle_pull_migration();

    source_workers.stop_and_join();
    for occupiers in remote_occupiers {
        occupiers.stop_and_join();
    }
    task_test_hooks::set_current_fair_periodic_balance(true)
        .expect("the test owner must restore periodic Fair balance");
    set_current_thread_affinity(CpuSet::all(cpu_count))
        .expect("test owner must restore full affinity");
    Ok(())
}
