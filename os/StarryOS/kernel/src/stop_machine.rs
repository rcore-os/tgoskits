use alloc::{boxed::Box, format, sync::Arc, vec::Vec};
use core::{
    hint::spin_loop,
    sync::atomic::{AtomicU8, AtomicUsize, Ordering},
};

use ax_lazyinit::LazyInit;
use ax_runtime::{
    hal::{cpu_num, percpu::this_cpu_id},
    task::{
        sched::{CpuId, CpuSet, SchedulePolicy},
        sync::WaitQueue,
    },
};

use crate::sync::{IrqMutex, Mutex, NoPreemptIrqSave, PreemptGuard};

static STOP_MACHINE_LOCK: Mutex<()> = Mutex::new(());
static CPU_STOPPERS: LazyInit<Vec<Arc<CpuStopper>>> = LazyInit::new();

const STAGE_PREPARE: u8 = 0;
const STAGE_DISABLE_IRQ: u8 = 1;
const STAGE_SYNC: u8 = 2;
const STAGE_EXIT: u8 = 3;

struct StopMachineState {
    stage: AtomicU8,
    prepared: AtomicUsize,
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
            stage: AtomicU8::new(STAGE_PREPARE),
            prepared: AtomicUsize::new(0),
            parked: AtomicUsize::new(0),
            finished: AtomicUsize::new(0),
            per_cpu_sync: Box::new(per_cpu_sync),
        }
    }
}

struct CpuStopper {
    command: IrqMutex<Option<Arc<StopMachineState>>>,
    ready: WaitQueue,
}

impl CpuStopper {
    const fn new() -> Self {
        Self {
            command: IrqMutex::new(None),
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
    let _preempt = PreemptGuard::new();
    // Like Linux MULTI_STOP_PREPARE, remain interruptible until every
    // participant has entered the stop callback and dispatch has completed.
    state.prepared.fetch_add(1, Ordering::Release);
    while state.stage.load(Ordering::Acquire) == STAGE_PREPARE {
        spin_loop();
    }

    let _guard = NoPreemptIrqSave::new();
    state.parked.fetch_add(1, Ordering::Release);
    while state.stage.load(Ordering::Acquire) == STAGE_DISABLE_IRQ {
        spin_loop();
    }

    (state.per_cpu_sync.as_ref())();
    state.finished.fetch_add(1, Ordering::Release);
    // The coordinator publishes exit only after all instruction-state
    // callbacks complete, so no participant resumes ordinary execution early.
    while state.stage.load(Ordering::Acquire) == STAGE_SYNC {
        spin_loop();
    }
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
        crate::task::kernel_thread_builder(format!("migration/{cpu}"))
            .policy(SchedulePolicy::kernel_stop())
            .affinity(affinity)
            .spawn(move || stopper.run())
            .expect("failed to spawn kernel thread");
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

    // Allocate before pinning the coordinator. After selecting the excluded
    // CPU, neither dispatch nor the barriers may sleep: resuming on a stopped
    // CPU would strand the only task able to release the remote stoppers.
    let mut remote_cpus = Vec::with_capacity(total_cpus);
    let state = Arc::new(StopMachineState::new(per_cpu_sync));
    let _coordinator = PreemptGuard::new();
    let current_cpu = this_cpu_id();
    remote_cpus.extend(
        (0..total_cpus)
            .filter(|&cpu| cpu != current_cpu && ax_runtime::hal::irq::is_cpu_online(cpu)),
    );
    let remote_cpu_count = remote_cpus.len();

    // Exercise the real scheduler context at the publication boundary. Sleeping
    // here can resume the coordinator on a CPU whose stopper is about to park.
    #[cfg(all(test, axtest))]
    assert!(
        ax_runtime::task::thread::current::validate_blocking_context().is_err(),
        "stop-machine coordinator can sleep after selecting the excluded CPU"
    );

    for &cpu_id in &remote_cpus {
        CPU_STOPPERS[cpu_id].submit(Arc::clone(&state));
    }

    while state.prepared.load(Ordering::Acquire) != remote_cpu_count {
        spin_loop();
    }

    {
        let _local_stop = NoPreemptIrqSave::new();
        state.stage.store(STAGE_DISABLE_IRQ, Ordering::Release);
        while state.parked.load(Ordering::Acquire) != remote_cpu_count {
            spin_loop();
        }
        let result = action();
        (state.per_cpu_sync.as_ref())();
        state.stage.store(STAGE_SYNC, Ordering::Release);
        while state.finished.load(Ordering::Acquire) != remote_cpu_count {
            spin_loop();
        }
        state.stage.store(STAGE_EXIT, Ordering::Release);
        result
    }
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
