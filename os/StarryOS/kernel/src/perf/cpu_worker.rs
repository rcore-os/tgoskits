//! Cross-CPU rendezvous for PMU operations owned by another CPU.
//!
//! Task-bound operations and sampling registration keep a fixed task-context
//! worker per CPU: they must join that CPU's scheduling order, or prepare and
//! reclaim owned IRQ-visible state in ordinary context. Pure counting register
//! transactions are bounded and allocation-free, so they run as synchronous hard
//! calls instead. An owner CPU whose only runnable task is a busy FIFO load still
//! services a remote `read`, enable, disable or reset, which a task-context
//! worker could not guarantee.

use alloc::{boxed::Box, collections::VecDeque, format, sync::Arc, vec::Vec};

use ax_lazyinit::LazyInit;
use ax_runtime::task::{
    sched::{CpuId, CpuSet},
    sync::WaitQueue,
};

use super::{
    hw::{
        self, SystemPmuConfigure, SystemPmuDisable, SystemPmuDisableResult, SystemPmuEnable,
        SystemPmuEnableResult, SystemPmuRead, SystemPmuReadResult, SystemPmuReplaceOutput,
        SystemPmuReset,
    },
    sampling_lifecycle::{Counter, PmuRunLease},
    target::PerfCpuId,
    task::{self, PerTaskCounter},
};
use crate::sync::{PreemptGuard, PreemptIrqSaveGuard, RawSpinLock};

const COMMAND_CAPACITY: usize = 64;

static CPU_WORKERS: LazyInit<Vec<Arc<PerfCpuWorker>>> = LazyInit::new();

struct PerfCompletion<T> {
    result: RawSpinLock<Option<crate::StarryResult<T>>>,
    waiters: WaitQueue,
}

impl<T> PerfCompletion<T> {
    const fn new() -> Self {
        Self {
            result: RawSpinLock::new(None),
            waiters: WaitQueue::new(),
        }
    }

    fn finish(&self, result: crate::StarryResult<T>) {
        let old = self.result.lock_irqsave().replace(result);
        assert!(old.is_none(), "perf CPU command completed twice");
        self.waiters.notify_all();
    }

    fn wait(&self) -> crate::StarryResult<T> {
        self.waiters
            .wait_until(|| self.result.lock_irqsave().is_some());
        self.result
            .lock_irqsave()
            .take()
            .expect("completed perf CPU command lost its result")
    }
}

enum PerfCpuCommand {
    Initialize {
        completion: Arc<PerfCompletion<()>>,
    },
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
    EnableSystem {
        request: Box<SystemPmuEnable>,
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
    ReplaceSystemOutput {
        request: SystemPmuReplaceOutput,
        completion: Arc<PerfCompletion<()>>,
    },
}

impl PerfCpuCommand {
    fn execute(self) {
        match self {
            Self::Initialize { completion } => {
                super::percpu::ensure_current_cpu_initialized();
                completion.finish(Ok(()));
            }
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
            Self::EnableSystem {
                request,
                completion,
            } => {
                let result = with_local_pmu_exclusion(|| hw::enable_system_on_owner(*request));
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
            Self::ReplaceSystemOutput {
                request,
                completion,
            } => {
                let result =
                    with_local_pmu_exclusion(|| hw::replace_system_output_on_owner(request));
                completion.finish(result);
            }
        }
    }
}

fn with_local_pmu_exclusion<T>(
    operation: impl FnOnce() -> crate::StarryResult<T>,
) -> crate::StarryResult<T> {
    let _guard = PreemptIrqSaveGuard::new();
    operation()
}

fn try_local<T>(
    owner: PerfCpuId,
    operation: impl FnOnce() -> crate::StarryResult<T>,
) -> Option<crate::StarryResult<T>> {
    let _guard = PreemptIrqSaveGuard::new();
    if owner.as_usize() != ax_runtime::hal::percpu::this_cpu_id() {
        return None;
    }
    Some(operation())
}

struct PerfCpuWorker {
    queue: RawSpinLock<VecDeque<PerfCpuCommand>>,
    ready: WaitQueue,
    space: WaitQueue,
}

impl PerfCpuWorker {
    fn new() -> Self {
        Self {
            queue: RawSpinLock::new(VecDeque::with_capacity(COMMAND_CAPACITY)),
            ready: WaitQueue::new(),
            space: WaitQueue::new(),
        }
    }

    fn submit(&self, command: PerfCpuCommand) {
        let mut command = Some(command);
        loop {
            {
                let mut queue = self.queue.lock_irqsave();
                if queue.len() < COMMAND_CAPACITY {
                    queue.push_back(command.take().expect("perf command submitted once"));
                    break;
                }
            }
            self.space
                .wait_until(|| self.queue.lock_irqsave().len() < COMMAND_CAPACITY);
        }
        self.ready.notify_one();
    }

    fn run(&self) -> ! {
        loop {
            self.ready
                .wait_until(|| !self.queue.lock_irqsave().is_empty());
            while let Some(command) = self.queue.lock_irqsave().pop_front() {
                self.space.notify_one();
                command.execute();
            }
        }
    }
}

fn owner_worker(owner: PerfCpuId) -> crate::StarryResult<&'static Arc<PerfCpuWorker>> {
    CPU_WORKERS
        .get()
        .and_then(|workers| workers.get(owner.as_usize()))
        .ok_or(crate::StarryError::BadState)
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
        crate::task::kernel_thread_builder(format!("perf-cpu/{cpu}"))
            .affinity(affinity)
            .spawn(move || worker.run())
            .expect("failed to spawn kernel thread");
    }

    for worker in CPU_WORKERS.iter() {
        let completion = Arc::new(PerfCompletion::new());
        worker.submit(PerfCpuCommand::Initialize {
            completion: Arc::clone(&completion),
        });
        completion
            .wait()
            .expect("perf CPU worker initialization failed");
    }
}

/// Forces the selected CPU through a scheduler boundary after task-event
/// publication.
///
/// Unlike register operations, this must not take the local fast path: when the
/// caller is the target task, sleeping until the fixed worker executes is what
/// guarantees the newly attached/enabled event receives a matching sched-in.
pub(super) fn synchronize_task_context(owner: PerfCpuId) -> crate::StarryResult<()> {
    let completion = Arc::new(PerfCompletion::new());
    owner_worker(owner)?.submit(PerfCpuCommand::SyncTaskContext {
        completion: Arc::clone(&completion),
    });
    completion.wait()
}

/// Stops one task-bound counter on the CPU that owns its running generation.
pub(super) fn stop_task_counter(
    counter: Arc<PerTaskCounter>,
    lease: PmuRunLease,
) -> crate::StarryResult<()> {
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
) -> crate::StarryResult<(u64, u64, u64)> {
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

/// One bounded pure-counting register transaction.
///
/// These requests carry no sampling registration and own no IRQ-visible
/// reference, so they can execute in the owner CPU's hard-call context.
enum CountingOperation {
    Configure(SystemPmuConfigure),
    Enable(Counter),
    Disable(Counter),
    Read(Counter),
    Reset(Counter),
}

/// Caller-owned request and outcome of one counting hard call.
struct CountingCall {
    operation: Option<CountingOperation>,
    failure: Option<crate::StarryError>,
    /// Counter value of a read or of a completed disable.
    value: u64,
    /// `started_at`, `observed_at` or `stopped_at` of the completed operation.
    timestamp: u64,
}

impl CountingCall {
    fn new(operation: CountingOperation) -> Self {
        Self {
            operation: Some(operation),
            failure: None,
            value: 0,
            timestamp: 0,
        }
    }

    fn finish(&mut self, outcome: crate::StarryResult<(u64, u64)>) {
        match outcome {
            Ok((value, timestamp)) => {
                self.value = value;
                self.timestamp = timestamp;
            }
            Err(error) => self.failure = Some(error),
        }
    }
}

/// Executes one counting transaction in the owner CPU's hard-call context.
///
/// # Safety
///
/// Only [`dispatch_counting_call`] reaches this function, and it keeps the
/// request pinned on its stack until the synchronous call returns.
unsafe fn counting_call_callback(argument: *mut ()) {
    // SAFETY: the caller upholds the pinned-request contract above.
    let call = unsafe { &mut *argument.cast::<CountingCall>() };
    let Some(operation) = call.operation.take() else {
        return;
    };
    let outcome = match operation {
        CountingOperation::Configure(request) => {
            hw::configure_system_on_owner(request).map(|()| (0, 0))
        }
        CountingOperation::Enable(counter) => hw::enable_system_on_owner(SystemPmuEnable {
            counter,
            sampling: None,
        })
        .map(|started| (0, started.started_at)),
        CountingOperation::Disable(counter) => hw::disable_system_on_owner(SystemPmuDisable {
            counter,
            registration: None,
        })
        .map(|stopped| (stopped.value, stopped.stopped_at)),
        CountingOperation::Read(counter) => hw::read_system_on_owner(SystemPmuRead {
            counter,
            sampling: None,
        })
        .map(|snapshot| (snapshot.value, snapshot.observed_at)),
        CountingOperation::Reset(counter) => hw::reset_system_on_owner(SystemPmuReset {
            counter,
            sampling_period: None,
        })
        .map(|()| (0, 0)),
    };
    call.finish(outcome);
}

fn map_rendezvous_error(error: ax_hal::irq::IrqError) -> crate::StarryError {
    use ax_hal::irq::IrqError;
    match error {
        IrqError::CpuOffline => crate::StarryError::NoSuchDeviceOrAddress,
        IrqError::InvalidCpu => crate::StarryError::InvalidInput,
        IrqError::Unsupported => crate::StarryError::Unsupported,
        IrqError::Timeout => crate::StarryError::TimedOut,
        _ => crate::StarryError::Io,
    }
}

/// Runs one counting transaction synchronously on `owner`.
///
/// A hard call executes inline when the caller already owns `owner`; otherwise
/// the request is serviced by an IPI, so progress never depends on the owner CPU
/// scheduling a worker. The caller keeps interrupts enabled while it waits.
fn dispatch_counting_call(
    owner: PerfCpuId,
    operation: CountingOperation,
) -> crate::StarryResult<CountingCall> {
    let mut call = CountingCall::new(operation);
    {
        // Pin the local fast-path CPU without masking IPIs, so a remote request
        // is still delivered while this caller waits for it.
        let _pin = PreemptGuard::new();
        // SAFETY: `call` is pinned on this stack for the whole synchronous
        // call, and the callback performs only bounded IRQ-safe PMU work that
        // reports its outcome on the same caller-owned request.
        let delivered = unsafe {
            ax_hal::irq::run_on_cpu_sync(
                ax_hal::irq::CpuId(owner.as_usize()),
                counting_call_callback,
                (&raw mut call).cast(),
            )
        };
        delivered.map_err(map_rendezvous_error)?;
    }
    match call.failure {
        Some(error) => Err(error),
        None => Ok(call),
    }
}

/// Configures one system-wide event on its target CPU.
///
/// Configuration precedes any sampling registration and is a bounded register
/// transaction, so it uses the synchronous rendezvous.
pub(super) fn configure_system(
    owner: PerfCpuId,
    request: SystemPmuConfigure,
) -> crate::StarryResult<()> {
    dispatch_counting_call(owner, CountingOperation::Configure(request)).map(|_| ())
}

/// Enables one system-wide event on its target CPU.
pub(super) fn enable_system(
    owner: PerfCpuId,
    request: SystemPmuEnable,
) -> crate::StarryResult<SystemPmuEnableResult> {
    if request.sampling.is_some() {
        return enable_sampling_system(owner, request);
    }
    let call = dispatch_counting_call(owner, CountingOperation::Enable(request.counter))?;
    Ok(SystemPmuEnableResult {
        registration: None,
        started_at: call.timestamp,
    })
}

/// Disables one system-wide event on its target CPU.
pub(super) fn disable_system(
    owner: PerfCpuId,
    request: SystemPmuDisable,
) -> crate::StarryResult<SystemPmuDisableResult> {
    if request.registration.is_some() {
        return disable_sampling_system(owner, request);
    }
    let call = dispatch_counting_call(owner, CountingOperation::Disable(request.counter))?;
    Ok(SystemPmuDisableResult {
        value: call.value,
        stopped_at: call.timestamp,
    })
}

/// Reads one system-wide event on its target CPU.
pub(super) fn read_system(
    owner: PerfCpuId,
    request: SystemPmuRead,
) -> crate::StarryResult<SystemPmuReadResult> {
    if request.sampling.is_some() {
        return read_sampling_system(owner, request);
    }
    let call = dispatch_counting_call(owner, CountingOperation::Read(request.counter))?;
    Ok(SystemPmuReadResult {
        value: call.value,
        observed_at: call.timestamp,
    })
}

/// Resets one system-wide event on its target CPU.
pub(super) fn reset_system(owner: PerfCpuId, request: SystemPmuReset) -> crate::StarryResult<()> {
    if request.sampling_period.is_some() {
        return reset_period_system(owner, request);
    }
    dispatch_counting_call(owner, CountingOperation::Reset(request.counter)).map(|_| ())
}

/// Rebinds an already-running system sampling slot to its newly mmap'd ring.
///
/// Replacing the registered slot migrates owned IRQ-visible output, so it stays
/// on the task-context worker.
pub(super) fn replace_system_output(
    owner: PerfCpuId,
    request: SystemPmuReplaceOutput,
) -> crate::StarryResult<()> {
    let mut request = Some(request);
    if let Some(result) = try_local(owner, || {
        hw::replace_system_output_on_owner(
            request.take().expect("single local PMU output replacement"),
        )
    }) {
        return result;
    }
    let completion = Arc::new(PerfCompletion::new());
    owner_worker(owner)?.submit(PerfCpuCommand::ReplaceSystemOutput {
        request: request.expect("remote PMU output replacement request"),
        completion: Arc::clone(&completion),
    });
    completion.wait()
}

/// Enables one system-wide sampling event on its target CPU.
///
/// Enabling registers a sampling slot, so it keeps the task-context worker that
/// prepares and owns that registration.
fn enable_sampling_system(
    owner: PerfCpuId,
    request: SystemPmuEnable,
) -> crate::StarryResult<SystemPmuEnableResult> {
    let mut request = Some(request);
    if let Some(result) = try_local(owner, || {
        hw::enable_system_on_owner(request.take().expect("single local PMU enable"))
    }) {
        return result;
    }
    let completion = Arc::new(PerfCompletion::new());
    owner_worker(owner)?.submit(PerfCpuCommand::EnableSystem {
        request: Box::new(request.expect("remote PMU enable request")),
        completion: Arc::clone(&completion),
    });
    completion.wait()
}

/// Disables one system-wide sampling event on its target CPU.
///
/// Unregistering its slot releases owned IRQ-visible state, so it stays on the
/// task-context worker.
fn disable_sampling_system(
    owner: PerfCpuId,
    request: SystemPmuDisable,
) -> crate::StarryResult<SystemPmuDisableResult> {
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

/// Reads one system-wide sampling event on its target CPU.
fn read_sampling_system(
    owner: PerfCpuId,
    request: SystemPmuRead,
) -> crate::StarryResult<SystemPmuReadResult> {
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

/// Reloads the logical period of one system-wide sampling event.
fn reset_period_system(owner: PerfCpuId, request: SystemPmuReset) -> crate::StarryResult<()> {
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
