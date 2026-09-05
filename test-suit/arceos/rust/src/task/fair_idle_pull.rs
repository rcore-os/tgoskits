use std::{
    os::arceos::{
        api::task::{AxCpuMask, ax_set_current_affinity},
        modules::ax_hal::percpu::this_cpu_id,
        task::{self as scheduler, CpuId, CpuSet, set_current_thread_affinity},
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

const SOURCE_WORKERS: usize = 4;
const PROGRESS_TIMEOUT: Duration = Duration::from_secs(10);
const NOHZ_KICK_TIMEOUT: Duration = Duration::from_millis(250);
const IDLE_SETTLE_TIME: Duration = Duration::from_millis(20);
const TEST_STACK_SIZE: usize = 256 * 1024;

struct CooperativeWorkers {
    stop: Arc<AtomicBool>,
    ready: Arc<AtomicUsize>,
    worker_count: usize,
    handles: Vec<scheduler::ThreadHandle>,
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
        let mut handles = Vec::with_capacity(worker_count);
        for _ in 0..worker_count {
            let stop = Arc::clone(&stop);
            let ready = Arc::clone(&ready);
            let observed_cpus = Arc::clone(&observed_cpus);
            let mut affinity = CpuSet::empty(cpu_count);
            assert!(affinity.insert(CpuId::new(cpu as u32)));
            handles.push(
                scheduler::spawn_raw_with_affinity(
                    move || {
                        assert_eq!(
                            this_cpu_id(),
                            cpu,
                            "pre-publication affinity must place the worker on its source CPU"
                        );
                        if expand_affinity {
                            set_current_thread_affinity(CpuSet::all(cpu_count))
                                .expect("source worker must widen its affinity in place");
                        }
                        ready.fetch_add(1, Ordering::Release);
                        while !stop.load(Ordering::Acquire) {
                            observed_cpus.fetch_or(1usize << this_cpu_id(), Ordering::Relaxed);
                            thread::yield_now();
                        }
                    },
                    String::from("fair-idle-pull-worker"),
                    TEST_STACK_SIZE,
                    affinity,
                )
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
            scheduler::join_thread(handle).expect("cooperative Fair worker must exit");
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

    let observed_cpus = Arc::new(AtomicUsize::new(0));
    let ignored_cpus = Arc::new(AtomicUsize::new(0));
    let mut remote_occupiers = Vec::with_capacity(cpu_count - 1);
    for cpu in 1..cpu_count {
        let occupier =
            CooperativeWorkers::spawn_pinned(cpu, cpu_count, 1, false, Arc::clone(&ignored_cpus));
        occupier.wait_ready();
        remote_occupiers.push(occupier);
    }

    // CPU1 first completes an idle entry while no source backlog exists. A
    // later false-to-true Fair pushable transition on CPU0 must kick this idle
    // owner like Linux `nohz_balancer_kick()`; waiting for the periodic Fair
    // balance deadline is only a fallback.
    remote_occupiers.remove(0).stop_and_join();
    thread::sleep(IDLE_SETTLE_TIME);

    let source_workers = CooperativeWorkers::spawn_pinned(
        0,
        cpu_count,
        SOURCE_WORKERS,
        true,
        Arc::clone(&observed_cpus),
    );
    source_workers.wait_ready();

    let started = Instant::now();
    while observed_cpus.load(Ordering::Acquire) & (1usize << 1) == 0
        && started.elapsed() < NOHZ_KICK_TIMEOUT
    {
        thread::yield_now();
    }
    assert!(
        observed_cpus.load(Ordering::Acquire) & (1usize << 1) != 0,
        "late Fair backlog did not kick an already idle CPU before periodic balance fallback"
    );

    source_workers.stop_and_join();
    for occupier in remote_occupiers {
        occupier.stop_and_join();
    }
    set_current_thread_affinity(CpuSet::all(cpu_count))
        .expect("test owner must restore full affinity");
    Ok(())
}
