//! CPU0 competitor retained through the complete guest sampling interval.

use std::{
    hint::black_box,
    sync::{
        Arc, Barrier,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
};

use anyhow::{Context, Result, ensure};

pub struct PerformanceLoad {
    stop: Arc<AtomicBool>,
    exited: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl PerformanceLoad {
    pub fn start() -> Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let exited = Arc::new(AtomicBool::new(false));
        let ready = Arc::new(Barrier::new(2));
        let worker_stop = stop.clone();
        let worker_exited = exited.clone();
        let worker_ready = ready.clone();
        let worker = std::thread::Builder::new()
            .name("vcpu-perf-load".into())
            .spawn(move || {
                use ax_std::os::arceos::api::task::{AxCpuMask, ax_set_current_affinity};
                ax_set_current_affinity(AxCpuMask::one_shot(0))
                    .expect("performance load CPU0 affinity");
                println!("VCPU_PERF_LOAD_READY cpu=0");
                worker_ready.wait();
                let mut checksum = 1u64;
                while !worker_stop.load(Ordering::Acquire) {
                    for _ in 0..1024 {
                        checksum =
                            black_box(checksum.wrapping_mul(6364136223846793005).wrapping_add(1));
                    }
                    std::thread::yield_now();
                }
                worker_exited.store(true, Ordering::Release);
            })
            .context("spawn CPU0 contention worker")?;
        ready.wait();
        Ok(Self {
            stop,
            exited,
            worker: Some(worker),
        })
    }

    pub fn check_running(&self) -> Result<()> {
        ensure!(
            !self.exited.load(Ordering::Acquire),
            "host competitor ended before sampling completed"
        );
        Ok(())
    }

    pub fn finish(mut self) -> Result<()> {
        self.check_running()?;
        self.join()
    }

    fn join(&mut self) -> Result<()> {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            worker
                .join()
                .map_err(|_| anyhow::anyhow!("host competitor panicked"))?;
        }
        Ok(())
    }
}

impl Drop for PerformanceLoad {
    fn drop(&mut self) {
        if let Err(error) = self.join() {
            println!("VCPU_PERF_FAIL {error:#}");
        }
    }
}
