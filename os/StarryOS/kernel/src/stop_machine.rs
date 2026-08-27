use alloc::{boxed::Box, sync::Arc};
use core::{
    hint::spin_loop,
    sync::atomic::{AtomicU8, AtomicUsize, Ordering},
};

use ax_ipi::legacy::run_on_cpu;
use ax_runtime::hal::{cpu_num, percpu::this_cpu_id, time::monotonic_time_nanos};

use crate::sync::{IrqMutex, PreemptIrqSaveGuard};

static STOP_MACHINE_LOCK: IrqMutex<()> = IrqMutex::new(());

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
    let _guard = PreemptIrqSaveGuard::new();

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
        run_on_cpu(cpu_id, move || park_remote_cpu(state))
            .unwrap_or_else(|err| panic!("stop_machine: failed to park CPU {cpu_id}: {err:?}"));
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

#[cfg(all(test, axtest))]
fn stop_machine_runs_action_and_sync_on_each_cpu_for_test() -> bool {
    let action_count = AtomicUsize::new(0);
    let sync_count = Arc::new(AtomicUsize::new(0));
    let remote_sync_count = sync_count.clone();

    stop_machine(
        || {
            action_count.fetch_add(1, Ordering::Relaxed);
        },
        move || {
            remote_sync_count.fetch_add(1, Ordering::Relaxed);
        },
    );

    action_count.load(Ordering::Relaxed) == 1 && sync_count.load(Ordering::Relaxed) == cpu_num()
}

#[cfg(test)]
mod tests {
    #[cfg(all(test, axtest))]
    #[axtest::axtest]
    fn runs_action_and_sync_on_each_cpu() {
        assert!(super::stop_machine_runs_action_and_sync_on_each_cpu_for_test());
    }
}
