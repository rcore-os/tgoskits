//! Fixed task-context workers for CPU-owned PMU operations.

use alloc::{collections::VecDeque, format, sync::Arc, vec::Vec};

use ax_errno::{AxError, AxResult};
use ax_kernel_guard::NoPreemptIrqSave;
use ax_kspin::SpinNoIrq;
use ax_lazyinit::LazyInit;
use ax_runtime::task::{CpuId, CpuSet, WaitQueue};

use super::{
    hw::{
        self, SystemPmuConfigure, SystemPmuDisable, SystemPmuDisableResult, SystemPmuEnable,
        SystemPmuEnableResult, SystemPmuRead, SystemPmuReadResult, SystemPmuReset,
    },
    sampling_lifecycle::PmuRunLease,
    target::PerfCpuId,
    task::{self, PerTaskCounter},
};

const COMMAND_CAPACITY: usize = 64;

static CPU_WORKERS: LazyInit<Vec<Arc<PerfCpuWorker>>> = LazyInit::new();

struct PerfCompletion<T> {
    result: SpinNoIrq<Option<AxResult<T>>>,
    waiters: WaitQueue,
}

impl<T> PerfCompletion<T> {
    const fn new() -> Self {
        Self {
            result: SpinNoIrq::new(None),
            waiters: WaitQueue::new(),
        }
    }

    fn finish(&self, result: AxResult<T>) {
        let old = self.result.lock().replace(result);
        assert!(old.is_none(), "perf CPU command completed twice");
        self.waiters.notify_all();
    }

    fn wait(&self) -> AxResult<T> {
        self.waiters.wait_until(|| self.result.lock().is_some());
        self.result
            .lock()
            .take()
            .expect("completed perf CPU command lost its result")
    }
}

enum PerfCpuCommand {
    SyncTaskContext {
        completion: Arc<PerfCompletion<()>>,
    },
    StopTask {
        counter: Arc<PerTaskCounter>,
        lease: PmuRunLease,
        completion: Arc<PerfCompletion<()>>,
    },
    ReadTask {
        counter: Arc<PerTaskCounter>,
        completion: Arc<PerfCompletion<(u64, u64, u64)>>,
    },
    ConfigureSystem {
        request: SystemPmuConfigure,
        completion: Arc<PerfCompletion<()>>,
    },
    EnableSystem {
        request: SystemPmuEnable,
        completion: Arc<PerfCompletion<SystemPmuEnableResult>>,
    },
    DisableSystem {
        request: SystemPmuDisable,
        completion: Arc<PerfCompletion<SystemPmuDisableResult>>,
    },
    ReadSystem {
        request: SystemPmuRead,
        completion: Arc<PerfCompletion<SystemPmuReadResult>>,
    },
    ResetSystem {
        request: SystemPmuReset,
        completion: Arc<PerfCompletion<()>>,
    },
}

impl PerfCpuCommand {
    fn execute(self) {
        match self {
            Self::SyncTaskContext { completion } => {
                // Reaching this fixed per-CPU worker proves that a task which
                // was running when the command was published crossed a
                // scheduler switch boundary. Its next switch-in observes all
                // perf counters published before this command.
                completion.finish(Ok(()));
            }
            Self::StopTask {
                counter,
                lease,
                completion,
            } => {
                let result =
                    with_local_pmu_exclusion(|| task::stop_requested_on_owner(&counter, lease));
                completion.finish(result);
            }
            Self::ReadTask {
                counter,
                completion,
            } => {
                let result = with_local_pmu_exclusion(|| task::read_task_on_owner(&counter));
                completion.finish(result);
            }
            Self::ConfigureSystem {
                request,
                completion,
            } => {
                let result = with_local_pmu_exclusion(|| hw::configure_system_on_owner(request));
                completion.finish(result);
            }
            Self::EnableSystem {
                request,
                completion,
            } => {
                let result = with_local_pmu_exclusion(|| hw::enable_system_on_owner(request));
                completion.finish(result);
            }
            Self::DisableSystem {
                request,
                completion,
            } => {
                let result = with_local_pmu_exclusion(|| hw::disable_system_on_owner(request));
                completion.finish(result);
            }
            Self::ReadSystem {
                request,
                completion,
            } => {
                let result = with_local_pmu_exclusion(|| hw::read_system_on_owner(request));
                completion.finish(result);
            }
            Self::ResetSystem {
                request,
                completion,
            } => {
                let result = with_local_pmu_exclusion(|| hw::reset_system_on_owner(request));
                completion.finish(result);
            }
        }
    }
}

fn with_local_pmu_exclusion<T>(operation: impl FnOnce() -> AxResult<T>) -> AxResult<T> {
    let _guard = NoPreemptIrqSave::new();
    operation()
}

fn try_local<T>(owner: PerfCpuId, operation: impl FnOnce() -> AxResult<T>) -> Option<AxResult<T>> {
    let _guard = NoPreemptIrqSave::new();
    if owner.as_usize() != ax_runtime::hal::percpu::this_cpu_id() {
        return None;
    }
    Some(operation())
}

struct PerfCpuWorker {
    queue: SpinNoIrq<VecDeque<PerfCpuCommand>>,
    ready: WaitQueue,
    space: WaitQueue,
}

impl PerfCpuWorker {
    fn new() -> Self {
        Self {
            queue: SpinNoIrq::new(VecDeque::with_capacity(COMMAND_CAPACITY)),
            ready: WaitQueue::new(),
            space: WaitQueue::new(),
        }
    }

    fn submit(&self, command: PerfCpuCommand) {
        let mut command = Some(command);
        loop {
            {
                let mut queue = self.queue.lock();
                if queue.len() < COMMAND_CAPACITY {
                    queue.push_back(command.take().expect("perf command submitted once"));
                    break;
                }
            }
            self.space
                .wait_until(|| self.queue.lock().len() < COMMAND_CAPACITY);
        }
        self.ready.notify_one();
    }

    fn run(&self) -> ! {
        loop {
            self.ready.wait_until(|| !self.queue.lock().is_empty());
            while let Some(command) = self.queue.lock().pop_front() {
                self.space.notify_one();
                command.execute();
            }
        }
    }
}

fn owner_worker(owner: PerfCpuId) -> AxResult<&'static Arc<PerfCpuWorker>> {
    CPU_WORKERS
        .get()
        .and_then(|workers| workers.get(owner.as_usize()))
        .ok_or(AxError::BadState)
}

/// Starts one fixed worker per online logical CPU.
pub(super) fn init() {
    let cpu_count = ax_runtime::hal::cpu_num();
    let workers: Vec<_> = (0..cpu_count)
        .map(|_| Arc::new(PerfCpuWorker::new()))
        .collect();
    CPU_WORKERS.init_once(workers);

    for cpu in 0..cpu_count {
        let worker = Arc::clone(&CPU_WORKERS[cpu]);
        let mut affinity = CpuSet::empty(cpu_count);
        assert!(affinity.insert(CpuId::new(cpu as u32)));
        crate::task::spawn_kernel_thread_with_affinity(
            move || worker.run(),
            format!("perf-cpu/{cpu}"),
            affinity,
        );
    }
}

/// Forces the selected CPU through a scheduler boundary after task-event
/// publication.
///
/// Unlike register operations, this must not take the local fast path: when the
/// caller is the target task, sleeping until the fixed worker executes is what
/// guarantees the newly attached/enabled event receives a matching sched-in.
pub(super) fn synchronize_task_context(owner: PerfCpuId) -> AxResult<()> {
    let completion = Arc::new(PerfCompletion::new());
    owner_worker(owner)?.submit(PerfCpuCommand::SyncTaskContext {
        completion: Arc::clone(&completion),
    });
    completion.wait()
}

/// Stops one task-bound counter on the CPU that owns its running generation.
pub(super) fn stop_task_counter(counter: Arc<PerTaskCounter>, lease: PmuRunLease) -> AxResult<()> {
    let owner = lease.owner();
    if let Some(result) = try_local(owner, || task::stop_requested_on_owner(&counter, lease)) {
        return result;
    }
    let completion = Arc::new(PerfCompletion::new());
    owner_worker(owner)?.submit(PerfCpuCommand::StopTask {
        counter,
        lease,
        completion: Arc::clone(&completion),
    });
    completion.wait()
}

/// Reads a task-bound counter after joining the owner CPU's scheduling order.
pub(super) fn read_task_counter(
    counter: Arc<PerTaskCounter>,
    owner: PerfCpuId,
) -> AxResult<(u64, u64, u64)> {
    if let Some(result) = try_local(owner, || task::read_task_on_owner(&counter)) {
        return result;
    }
    let completion = Arc::new(PerfCompletion::new());
    owner_worker(owner)?.submit(PerfCpuCommand::ReadTask {
        counter,
        completion: Arc::clone(&completion),
    });
    completion.wait()
}

/// Configures one system-wide event on its target CPU.
pub(super) fn configure_system(owner: PerfCpuId, request: SystemPmuConfigure) -> AxResult<()> {
    let mut request = Some(request);
    if let Some(result) = try_local(owner, || {
        hw::configure_system_on_owner(request.take().expect("single local PMU configure"))
    }) {
        return result;
    }
    let completion = Arc::new(PerfCompletion::new());
    owner_worker(owner)?.submit(PerfCpuCommand::ConfigureSystem {
        request: request.expect("remote PMU configure request"),
        completion: Arc::clone(&completion),
    });
    completion.wait()
}

/// Enables one system-wide event on its target CPU.
pub(super) fn enable_system(
    owner: PerfCpuId,
    request: SystemPmuEnable,
) -> AxResult<SystemPmuEnableResult> {
    let mut request = Some(request);
    if let Some(result) = try_local(owner, || {
        hw::enable_system_on_owner(request.take().expect("single local PMU enable"))
    }) {
        return result;
    }
    let completion = Arc::new(PerfCompletion::new());
    owner_worker(owner)?.submit(PerfCpuCommand::EnableSystem {
        request: request.expect("remote PMU enable request"),
        completion: Arc::clone(&completion),
    });
    completion.wait()
}

/// Disables one system-wide event on its target CPU.
pub(super) fn disable_system(
    owner: PerfCpuId,
    request: SystemPmuDisable,
) -> AxResult<SystemPmuDisableResult> {
    let mut request = Some(request);
    if let Some(result) = try_local(owner, || {
        hw::disable_system_on_owner(request.take().expect("single local PMU disable"))
    }) {
        return result;
    }
    let completion = Arc::new(PerfCompletion::new());
    owner_worker(owner)?.submit(PerfCpuCommand::DisableSystem {
        request: request.expect("remote PMU disable request"),
        completion: Arc::clone(&completion),
    });
    completion.wait()
}

/// Reads one system-wide event on its target CPU.
pub(super) fn read_system(
    owner: PerfCpuId,
    request: SystemPmuRead,
) -> AxResult<SystemPmuReadResult> {
    let mut request = Some(request);
    if let Some(result) = try_local(owner, || {
        hw::read_system_on_owner(request.take().expect("single local PMU read"))
    }) {
        return result;
    }
    let completion = Arc::new(PerfCompletion::new());
    owner_worker(owner)?.submit(PerfCpuCommand::ReadSystem {
        request: request.expect("remote PMU read request"),
        completion: Arc::clone(&completion),
    });
    completion.wait()
}

/// Resets one system-wide event on its target CPU.
pub(super) fn reset_system(owner: PerfCpuId, request: SystemPmuReset) -> AxResult<()> {
    let mut request = Some(request);
    if let Some(result) = try_local(owner, || {
        hw::reset_system_on_owner(request.take().expect("single local PMU reset"))
    }) {
        return result;
    }
    let completion = Arc::new(PerfCompletion::new());
    owner_worker(owner)?.submit(PerfCpuCommand::ResetSystem {
        request: request.expect("remote PMU reset request"),
        completion: Arc::clone(&completion),
    });
    completion.wait()
}
