use alloc::{boxed::Box, format, sync::Arc, vec::Vec};
use core::{
    hint::spin_loop,
    sync::atomic::{AtomicU8, AtomicUsize, Ordering},
};

use ax_lazyinit::LazyInit;
use ax_runtime::{
    hal::{cpu_num, percpu::this_cpu_id},
    task::{CpuId, CpuSet, SchedulePolicy, WaitQueue},
};

use crate::sync::{NoPreemptIrqSave, PiMutex};

static STOP_MACHINE_LOCK: PiMutex<()> = PiMutex::new(());
static CPU_STOPPERS: LazyInit<Vec<Arc<CpuStopper>>> = LazyInit::new();

const STAGE_PARKED: u8 = 0;
const STAGE_SYNC: u8 = 1;

struct StopMachineState {
    stage: AtomicU8,
    parked: AtomicUsize,
    finished: AtomicUsize,
    progress: WaitQueue,
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
            progress: WaitQueue::new(),
            per_cpu_sync: Box::new(per_cpu_sync),
        }
    }
}

struct CpuStopper {
    command: PiMutex<Option<Arc<StopMachineState>>>,
    ready: WaitQueue,
}

impl CpuStopper {
    const fn new() -> Self {
        Self {
            command: PiMutex::new(None),
            ready: WaitQueue::new(),
        }
    }

    fn submit(&self, state: Arc<StopMachineState>) {
        let replaced = self.command.lock().replace(state);
        assert!(
            replaced.is_none(),
            "CPU stopper accepted overlapping commands"
        );
        self.ready.notify_one();
    }

    fn run(&self) -> ! {
        loop {
            self.ready.wait_until(|| self.command.lock().is_some());
            let state = self
                .command
                .lock()
                .take()
                .expect("notified CPU stopper lost its command");
            park_remote_cpu(&state);
        }
    }
}

fn park_remote_cpu(state: &StopMachineState) {
    {
        let _guard = NoPreemptIrqSave::new();

        state.parked.fetch_add(1, Ordering::SeqCst);
        state.progress.notify_one();
        while state.stage.load(Ordering::Acquire) == STAGE_PARKED {
            spin_loop();
        }

        (state.per_cpu_sync.as_ref())();
        state.finished.fetch_add(1, Ordering::Release);
    }
    state.progress.notify_one();
}

/// Starts one persistent stopper task per online logical CPU.
pub(crate) fn init() {
    let cpu_count = cpu_num();
    let stoppers: Vec<_> = (0..cpu_count)
        .map(|_| Arc::new(CpuStopper::new()))
        .collect();
    CPU_STOPPERS.init_once(stoppers);

    for cpu in 0..cpu_count {
        let stopper = Arc::clone(&CPU_STOPPERS[cpu]);
        let mut affinity = CpuSet::empty(cpu_count);
        assert!(affinity.insert(CpuId::new(cpu as u32)));
        crate::task::spawn_kernel_thread_with_policy_and_affinity(
            move || stopper.run(),
            format!("migration/{cpu}"),
            SchedulePolicy::kernel_stop(),
            affinity,
        );
    }
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
        let _local_stop = NoPreemptIrqSave::new();
        let result = action();
        per_cpu_sync();
        return result;
    }

    let current_cpu = this_cpu_id();
    let remote_cpus: Vec<_> = (0..total_cpus)
        .filter(|&cpu| cpu != current_cpu && ax_runtime::hal::irq::is_cpu_online(cpu))
        .collect();
    let remote_cpu_count = remote_cpus.len();
    let state = Arc::new(StopMachineState::new(per_cpu_sync));

    for &cpu_id in &remote_cpus {
        CPU_STOPPERS[cpu_id].submit(Arc::clone(&state));
    }

    state
        .progress
        .wait_until(|| state.parked.load(Ordering::Acquire) == remote_cpu_count);

    let result = {
        let _local_stop = NoPreemptIrqSave::new();
        let result = action();
        (state.per_cpu_sync.as_ref())();
        state.stage.store(STAGE_SYNC, Ordering::Release);
        result
    };

    state
        .progress
        .wait_until(|| state.finished.load(Ordering::Acquire) == remote_cpu_count);

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

#[cfg(all(test, axtest))]
mod tests {
    #[cfg(all(test, axtest))]
    #[axtest::axtest]
    fn runs_action_and_sync_on_each_cpu() {
        assert!(super::stop_machine_runs_action_and_sync_on_each_cpu_for_test());
    }
}
