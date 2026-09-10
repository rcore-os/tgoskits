//! Bounded test workload, independent of the network and console services.

use std::{
    hint::black_box,
    sync::{
        Arc, Barrier,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

pub(crate) struct PerformanceLoad {
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

pub(crate) fn start() -> PerformanceLoad {
    let stop = Arc::new(AtomicBool::new(false));
    let ready = Arc::new(Barrier::new(2));
    let worker_stop = stop.clone();
    let worker_ready = ready.clone();
    let worker = std::thread::Builder::new()
        .name("vcpu-perf-load".into())
        .spawn(move || {
            use ax_std::os::arceos::api::task::{AxCpuMask, ax_set_current_affinity};
            ax_set_current_affinity(AxCpuMask::one_shot(0)).expect("performance load CPU 0 exists");
            println!("VCPU_PERF_LOAD_READY cpu=0");
            worker_ready.wait();
            let started = Instant::now();
            let mut checksum = 1u64;
            while !worker_stop.load(Ordering::Acquire)
                && started.elapsed() < Duration::from_secs(60)
            {
                for _ in 0..1024 {
                    checksum =
                        black_box(checksum.wrapping_mul(6364136223846793005).wrapping_add(1));
                }
                std::thread::yield_now();
            }
            println!("VCPU_PERF_LOAD_STOPPED checksum={checksum}");
        })
        .expect("performance load thread creation");
    ready.wait();
    PerformanceLoad {
        stop,
        worker: Some(worker),
    }
}

impl Drop for PerformanceLoad {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            worker.join().expect("performance load thread exit");
        }
    }
}
