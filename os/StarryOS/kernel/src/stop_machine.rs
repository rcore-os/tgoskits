use alloc::{boxed::Box, sync::Arc};
use core::{
    hint::spin_loop,
    sync::atomic::{AtomicU8, AtomicUsize, Ordering},
};

use ax_ipi::run_on_cpu;
use ax_kspin::{PreemptIrqGuard, SpinNoIrq};
use ax_runtime::hal::{cpu_num, percpu::this_cpu_id, time::monotonic_time_nanos};

static STOP_MACHINE_LOCK: SpinNoIrq<()> = SpinNoIrq::new(());

const STAGE_PARKED: u8 = 0;
const STAGE_SYNC: u8 = 1;

struct StopMachineState {
    stage: AtomicU8,
    parked: AtomicUsize,
    finished: AtomicUsize,
    per_cpu_sync: Box<dyn Fn() + Send + Sync>,
}

impl StopMachineState {
    fn new<F>(per_cpu_sync: F) -> Self
    where
        F: Fn() + Send + Sync + 'static,
    {
        Self {
            stage: AtomicU8::new(STAGE_PARKED),
            parked: AtomicUsize::new(0),
            finished: AtomicUsize::new(0),
            per_cpu_sync: Box::new(per_cpu_sync),
        }
    }
}

fn park_remote_cpu(state: Arc<StopMachineState>) {
    let _guard = PreemptIrqGuard::new();

    state.parked.fetch_add(1, Ordering::SeqCst);
    while state.stage.load(Ordering::SeqCst) == STAGE_PARKED {
        spin_loop();
    }

    (state.per_cpu_sync.as_ref())();
    state.finished.fetch_add(1, Ordering::SeqCst);
}

/// Run a short non-blocking critical section while all other CPUs are parked.
///
/// Both `action` and `per_cpu_sync` must not sleep or fault, and may only take
/// IRQ-safe locks.
pub(crate) fn stop_machine<R, A, S>(action: A, per_cpu_sync: S) -> R
where
    A: FnOnce() -> R,
    S: Fn() + Send + Sync + 'static,
{
    let _lock = STOP_MACHINE_LOCK.lock();
    let total_cpus = cpu_num();

    if total_cpus <= 1 {
        let result = action();
        per_cpu_sync();
        return result;
    }

    let current_cpu = this_cpu_id();
    let remote_cpu_count = total_cpus - 1;
    let state = Arc::new(StopMachineState::new(per_cpu_sync));

    for cpu_id in 0..total_cpus {
        if cpu_id == current_cpu {
            continue;
        }

        let state = state.clone();
        run_on_cpu(cpu_id, move || park_remote_cpu(state));
    }

    const MAX_WAIT_NS: u64 = 5_000_000_000; // 5 seconds
    let now = monotonic_time_nanos();
    while state.parked.load(Ordering::SeqCst) != remote_cpu_count {
        spin_loop();
        if monotonic_time_nanos() - now > MAX_WAIT_NS {
            panic!("stop_machine: timeout waiting for remote CPUs to park");
        }
    }

    // Now all remote CPUs are parked. We can safely execute the critical section.
    let result = action();
    (state.per_cpu_sync.as_ref())();
    state.stage.store(STAGE_SYNC, Ordering::SeqCst);

    while state.finished.load(Ordering::SeqCst) != remote_cpu_count {
        spin_loop();
    }

    result
}
