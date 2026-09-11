//! Starry ownership adapter for runtime-backed scheduler threads.

use alloc::{boxed::Box, string::String, sync::Arc};
#[cfg(axtest)]
use core::sync::atomic::AtomicUsize;
use core::{
    ptr,
    sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering},
    task::{Context, Poll},
    time::Duration,
};

use ax_std::os::arceos::task as scheduler;

use super::{PidIdentity, PidSnapshot, Thread};
#[cfg(target_arch = "aarch64")]
use super::{PidNamespaceId, TgidNumber, TidNumber};
use crate::sync::{Mutex, NoPreemptIrqSave};

const TASK_COMM_LEN: usize = 16;

#[ax_percpu::def_percpu]
static CURRENT_USER_EXTENSION: usize = 0;

static CURRENT_USER_VIEW_FAILURES: AtomicU64 = AtomicU64::new(0);

/// Strong Starry user-task reference backed by a checked scheduler handle.
#[derive(Clone, Debug)]
pub struct UserTaskRef {
    scheduler: scheduler::thread::ThreadHandle,
    extension_data: usize,
}

/// Starry user task whose scheduler record exists but cannot run yet.
///
/// Clone and pthread creation use this transaction token to finish all private
/// resources before fallible scheduler staging.
pub struct PreparedUserTask {
    scheduler: scheduler::thread::PreparedThread,
    extension_data: usize,
}

/// Starry task whose fallible scheduler placement has completed while its
/// scheduler state remains New until Linux identity publication commits.
pub struct StagedUserTask {
    scheduler: scheduler::thread::StagedThread,
    extension_data: usize,
}

impl PreparedUserTask {
    /// Runs pre-publication registry setup with a temporary strong task view.
    ///
    /// The view is always dropped before this method returns, so callers cannot
    /// accidentally retain an extra scheduler handle that would prevent
    /// [`Self::stage`] failure or token drop from reaping the prepared record.
    #[cfg(target_arch = "aarch64")]
    pub fn with_task<R>(&self, operation: impl FnOnce(&UserTaskRef) -> R) -> R {
        let task = finish_published_user_thread(self.scheduler.thread_handle());
        operation(&task)
    }

    /// Completes fallible scheduler placement while keeping the task in New state.
    pub fn stage(self) -> Result<StagedUserTask, scheduler::thread::TaskError> {
        Ok(StagedUserTask {
            scheduler: self.scheduler.stage()?,
            extension_data: self.extension_data,
        })
    }
}

impl StagedUserTask {
    /// Runs registry publication against the staged task's stable identity.
    pub fn with_task<R>(&self, operation: impl FnOnce(&UserTaskRef) -> R) -> R {
        let task = finish_published_user_thread(self.scheduler.thread_handle());
        operation(&task)
    }

    /// Commits first runqueue admission after all Linux-visible state is committed.
    pub fn activate(self) -> UserTaskRef {
        let extension_data = self.extension_data;
        let task = finish_published_user_thread(self.scheduler.activate());
        debug_assert_eq!(task.extension_data, extension_data);
        task
    }
}

impl UserTaskRef {
    /// Tries to recover a Starry user task from a generic scheduler thread.
    ///
    /// Threads without an OS extension or with a foreign extension
    /// return `Ok(None)`. Matching Starry operations with malformed data are a
    /// runtime-handle error.
    pub fn try_from_scheduler(
        handle: scheduler::thread::ThreadHandle,
    ) -> Result<Option<Self>, scheduler::thread::TaskError> {
        let Some(extension_data) = try_extension_data(&handle)? else {
            return Ok(None);
        };
        // SAFETY: `try_extension_data` validated the callback-table identity,
        // pointer alignment, and non-null value while `handle` pins the
        // scheduler-owned extension. The handle is retained by the returned adapter.
        let data = unsafe { extension_data_from_raw(extension_data) };
        data.thread
            .validate_scheduler_id(handle.id())
            .map_err(|_| scheduler::thread::TaskError::InvalidRuntimeHandle)?;
        Ok(Some(Self {
            scheduler: handle,
            extension_data,
        }))
    }

    /// Returns the generation-bearing scheduler identity.
    pub fn id(&self) -> scheduler::thread::ThreadId {
        self.scheduler.id()
    }

    /// Formats the scheduler identity and diagnostic name.
    pub fn id_name(&self) -> String {
        alloc::format!("Task({}, {:?})", self.id().as_u64(), self.name())
    }

    /// Returns the Starry thread attached through the checked extension.
    pub fn as_thread(&self) -> &Thread {
        &self.extension().thread
    }

    pub(crate) fn transfer_irq_pid_identity(
        &self,
        identity: &PidIdentity,
    ) -> crate::StarryResult<()> {
        self.extension()
            .irq_identity
            .transfer_to_process_identity(identity)
    }

    /// Returns a shared snapshot of the diagnostic task name.
    pub fn name(&self) -> Arc<String> {
        self.extension().name.lock().clone()
    }

    /// Replaces the Linux-visible thread command name.
    pub fn set_name(&self, name: &str) {
        let extension = self.extension();
        let replacement = Arc::new(String::from(name));
        let previous = {
            let mut stored_name = extension.name.lock();
            let previous = core::mem::replace(&mut *stored_name, replacement);
            extension.irq_identity.set_comm(name);
            previous
        };
        // The old snapshot may own the final allocation reference.
        drop(previous);
    }

    /// Returns whether Linux `RESET_ON_FORK` is active for this thread.
    pub fn reset_on_fork(&self) -> bool {
        self.extension().reset_on_fork.load(Ordering::Acquire)
    }

    /// Updates Linux `RESET_ON_FORK` metadata after policy validation.
    pub fn set_reset_on_fork(&self, reset: bool) {
        self.extension()
            .reset_on_fork
            .store(reset, Ordering::Release);
    }

    /// Commits an exec-time address-space replacement for the running thread.
    pub fn switch_address_space(
        &self,
        address_space: ax_std::os::arceos::thread::TaskAddressSpace,
    ) {
        assert_eq!(
            self.id(),
            scheduler::thread::current::current_thread_id()
                .unwrap_or_else(|error| panic!("page-table switch has no current task: {error}")),
            "only the running task may replace its page table"
        );
        ax_runtime::thread::switch_current_address_space(address_space)
            .unwrap_or_else(|error| panic!("failed to replace current address space: {error}"));
    }

    /// Creates a non-owning generation-checked task reference.
    pub fn downgrade(&self) -> WeakUserTaskRef {
        WeakUserTaskRef {
            scheduler_id: self.scheduler.id(),
        }
    }

    /// Creates a stable direct-wake handle for IRQ or remote producers.
    pub fn wake_handle(&self) -> scheduler::thread::ThreadWakeHandle {
        self.scheduler.wake_handle()
    }

    /// Returns the scheduler lifecycle snapshot.
    pub fn state(&self) -> scheduler::thread::ThreadState {
        self.scheduler.state()
    }

    /// Returns the last CPU selected for this task, if placement is known.
    pub fn assigned_cpu(&self) -> Option<usize> {
        self.scheduler
            .assigned_cpu()
            .map(|cpu| cpu.as_u32() as usize)
    }

    /// Returns the base scheduling policy.
    pub fn base_policy(&self) -> scheduler::sched::SchedulePolicy {
        self.scheduler.base_policy()
    }

    /// Returns the scheduler affinity snapshot.
    pub fn affinity(&self) -> scheduler::sched::CpuSet {
        self.scheduler
            .affinity()
            .unwrap_or_else(|error| panic!("failed to read Starry task affinity: {error}"))
    }

    /// Sets the Starry-local interruption bit and directly wakes this thread.
    pub fn interrupt(&self) {
        self.as_thread().interrupt();
        let _result = self.wake_handle().wake();
    }

    /// Tests and consumes one pending interruption.
    pub fn poll_interrupt(&self, _context: &Context<'_>) -> Poll<()> {
        if self.as_thread().take_interrupt() {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }

    /// Consumes one pending interruption from a synchronous user wait.
    pub(crate) fn take_interrupt(&self) -> bool {
        self.as_thread().take_interrupt()
    }

    /// Tests whether an interruption remains pending.
    pub fn interrupted(&self) -> bool {
        self.as_thread().interrupted()
    }

    /// Waits for exit and reaps the scheduler-owned runtime resources.
    pub fn join(self) -> i32 {
        self.scheduler
            .join()
            .unwrap_or_else(|error| panic!("failed to join Starry task: {error}"))
    }

    fn extension(&self) -> &StarryUserTaskExtension {
        // SAFETY: construction validates this value and retains the scheduler
        // handle that pins the scheduler-owned OS extension for `self`'s whole
        // lifetime. The callback table and data pointer are immutable.
        unsafe { extension_data_from_raw(self.extension_data) }
    }
}

impl PartialEq for UserTaskRef {
    fn eq(&self, other: &Self) -> bool {
        self.scheduler.id() == other.scheduler.id()
    }
}

impl Eq for UserTaskRef {}

/// Non-owning Starry task reference that cannot alias a reused registry slot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WeakUserTaskRef {
    scheduler_id: scheduler::thread::ThreadId,
}

impl WeakUserTaskRef {
    /// Upgrades the reference only while the same slot generation is live.
    pub fn upgrade(self) -> Result<Option<UserTaskRef>, scheduler::thread::TaskError> {
        let Some(handle) = resolve_weak_scheduler_handle(scheduler::thread::ThreadHandle::lookup(
            self.scheduler_id,
        ))?
        else {
            return Ok(None);
        };
        UserTaskRef::try_from_scheduler(handle)
    }
}

fn resolve_weak_scheduler_handle(
    lookup: Result<scheduler::thread::ThreadHandle, scheduler::thread::TaskError>,
) -> Result<Option<scheduler::thread::ThreadHandle>, scheduler::thread::TaskError> {
    match lookup {
        Ok(handle) => Ok(Some(handle)),
        Err(scheduler::thread::TaskError::StaleThreadId) => Ok(None),
        Err(error) => Err(error),
    }
}

/// Tries to recover a Starry user task for the calling scheduler thread.
pub fn try_current_user_task() -> Result<Option<UserTaskRef>, scheduler::thread::TaskError> {
    UserTaskRef::try_from_scheduler(scheduler::thread::current::current_thread_handle()?)
}

/// Returns the calling Starry user task.
///
/// A scheduler thread without a Starry extension is a kernel/runtime worker and
/// must not enter a Starry syscall or process path.
#[track_caller]
pub fn current_user_task() -> UserTaskRef {
    match try_current_user_task() {
        Ok(Some(task)) => task,
        Ok(None) => panic!("current scheduler thread is not a Starry user task"),
        Err(error) => panic!("failed to query current Starry user task: {error}"),
    }
}

/// A non-owning current-user view for trap, probe, and trace observers.
///
/// The embedded IRQ guard pins the current CPU and prevents the scheduler from
/// replacing or reaping the published extension until this view is dropped.
pub(crate) struct UserTaskIrqView {
    extension_data: usize,
    _irq_guard: NoPreemptIrqSave,
}

impl UserTaskIrqView {
    /// Returns the stable PID generation cached before the task became runnable.
    pub(crate) fn pid_identity_id(&self) -> u64 {
        self.extension()
            .irq_identity
            .thread_identity()
            .identity_id()
            .get()
    }

    /// Returns the Linux thread ID cached before the task became runnable.
    pub(crate) fn tid(&self) -> u32 {
        self.extension()
            .irq_identity
            .thread_identity()
            .root_number()
            .get()
    }

    /// Projects the current thread ID into `observer` without locks.
    #[cfg(target_arch = "aarch64")]
    pub(crate) fn visible_tid(&self, observer: PidNamespaceId) -> Option<TidNumber> {
        self.extension()
            .irq_identity
            .thread_identity()
            .visible_number(observer)
            .map(TidNumber::from)
    }

    /// Projects the thread-group ID into `observer` without locks.
    #[cfg(target_arch = "aarch64")]
    pub(crate) fn visible_tgid(&self, observer: PidNamespaceId) -> Option<TgidNumber> {
        self.extension()
            .irq_identity
            .process_identity
            .visible_number(observer)
            .map(TgidNumber::from)
    }

    /// Copies the lock-free Linux command-name snapshot into `output`.
    pub(crate) fn copy_comm(&self, output: &mut [u8; TASK_COMM_LEN]) -> Option<usize> {
        self.extension().irq_identity.copy_comm(output)
    }

    /// Pushes one return-probe instance without allocation or recursive spin.
    pub(crate) fn push_kretprobe(&self, instance: kprobe::retprobe::RetprobeInstance) {
        self.extension().thread.push_kretprobe(instance);
    }

    /// Pops one return-probe instance without allocation or recursive spin.
    pub(crate) fn pop_kretprobe(&self) -> kprobe::retprobe::RetprobeInstance {
        self.extension().thread.pop_kretprobe()
    }

    fn extension(&self) -> &StarryUserTaskExtension {
        // SAFETY: the per-CPU slot is written only by validated extension
        // switch hooks. The retained IRQ guard prevents a switch-out and the
        // scheduler keeps an on-CPU extension alive until the hook completes.
        unsafe { extension_data_from_raw(self.extension_data) }
    }
}

/// Acquires the Starry user task published for this CPU without a registry lookup.
///
/// Binding failures are counted in a fixed atomic diagnostic and fail closed;
/// observers must use a neutral kernel identity when this returns `None`.
pub(crate) fn try_current_user_irq_view() -> Option<UserTaskIrqView> {
    let irq_guard = NoPreemptIrqSave::new();
    // SAFETY: the retained guard prevents migration and local IRQ reentry for
    // both the scoped read and the returned view's lifetime.
    let extension_data = match unsafe {
        ax_runtime::hal::percpu::with_cpu_pin(|pin| CURRENT_USER_EXTENSION.read_current(pin))
    } {
        Ok(extension_data) => extension_data,
        Err(_) => {
            CURRENT_USER_VIEW_FAILURES.fetch_add(1, Ordering::Relaxed);
            return None;
        }
    };
    if extension_data == 0 {
        return None;
    }
    if !extension_data.is_multiple_of(core::mem::align_of::<StarryUserTaskExtension>()) {
        CURRENT_USER_VIEW_FAILURES.fetch_add(1, Ordering::Relaxed);
        return None;
    }
    Some(UserTaskIrqView {
        extension_data,
        _irq_guard: irq_guard,
    })
}

/// Returns the common thread builder with Starry's kernel stack size.
///
/// Callers configure policy or affinity before `spawn`, or use `prepare` to
/// retain the unpublished task. Completion and reclamation belong to the
/// returned runtime thread handle.
pub fn kernel_thread_builder(name: String) -> scheduler::thread::ThreadBuilder {
    ax_std::os::arceos::thread::builder(name).stack_size(crate::config::KERNEL_STACK_SIZE)
}

/// Yields the calling scheduler thread.
pub fn yield_now() {
    #[cfg(axtest)]
    YIELD_NOW_CALLS.fetch_add(1, Ordering::Relaxed);
    scheduler::thread::current::yield_current_cpu()
        .unwrap_or_else(|error| panic!("failed to yield current scheduler thread: {error}"));
}

#[cfg(axtest)]
static YIELD_NOW_CALLS: AtomicUsize = AtomicUsize::new(0);

#[cfg(axtest)]
pub(crate) fn reset_yield_now_calls_for_test() {
    YIELD_NOW_CALLS.store(0, Ordering::Relaxed);
}

#[cfg(axtest)]
pub(crate) fn yield_now_calls_for_test() -> usize {
    YIELD_NOW_CALLS.load(Ordering::Relaxed)
}

/// Sleeps the calling scheduler thread for at least `duration`.
pub fn sleep(duration: Duration) {
    scheduler::thread::current::sleep(duration);
}

/// Diagnoses an invalid attempt to sleep from hard-IRQ context.
#[track_caller]
pub fn might_sleep() {
    assert!(
        !ax_runtime::hal::irq::in_irq_context(),
        "sleeping operation entered from hard IRQ context"
    );
}

/// Prepares a Starry user thread without publishing its Linux identity.
///
/// The MM comes from the thread's process; options only configure its private
/// initial state. Stage reserves placement, and activate follows OS publication.
pub fn prepare_user_thread<F>(
    entry: F,
    thread: Thread,
    options: UserThreadOptions,
) -> Result<PreparedUserTask, scheduler::thread::TaskError>
where
    F: FnOnce() + Send + 'static,
{
    let address_space = thread.proc_data.scheduler_address_space()?;
    prepare_user_thread_inner(entry, thread, options, address_space)
}

/// Scheduling attributes committed atomically during user-thread creation.
pub struct UserThreadInitialSchedulerState {
    policy: scheduler::sched::SchedulePolicy,
    affinity: Option<scheduler::sched::CpuSet>,
    reset_on_fork: bool,
}

impl UserThreadInitialSchedulerState {
    /// Captures the scheduling attributes inherited by an unpublished child.
    pub fn new(
        policy: scheduler::sched::SchedulePolicy,
        affinity: scheduler::sched::CpuSet,
        reset_on_fork: bool,
    ) -> Self {
        Self {
            policy,
            affinity: Some(affinity),
            reset_on_fork,
        }
    }

    fn default_user() -> Self {
        Self {
            policy: scheduler::sched::SchedulePolicy::default(),
            affinity: None,
            reset_on_fork: false,
        }
    }
}

/// Private initial state for the common Starry user-thread preparation path.
///
/// The default starts with fresh FP state and the default scheduler policy.
/// Clone explicitly supplies inherited scheduling and architecture FP state.
pub struct UserThreadOptions {
    name: String,
    scheduler_state: UserThreadInitialSchedulerState,
    #[cfg(target_arch = "riscv64")]
    fp_state: Option<ax_cpu::registers::FpState>,
    #[cfg(not(target_arch = "riscv64"))]
    fp_initialization: FpInitialization,
}

impl UserThreadOptions {
    /// Copies the name before task publication and uses the default stack,
    /// scheduling policy and FP state. Allocation failure returns `NoMemory`.
    pub fn new(name: &str) -> Result<Self, scheduler::thread::TaskError> {
        Ok(Self {
            name: super::allocation::try_string(name)?,
            scheduler_state: UserThreadInitialSchedulerState::default_user(),
            #[cfg(target_arch = "riscv64")]
            fp_state: None,
            #[cfg(not(target_arch = "riscv64"))]
            fp_initialization: FpInitialization::Default,
        })
    }

    /// Installs the child's scheduling attributes before first activation.
    pub fn with_scheduler_state(mut self, state: UserThreadInitialSchedulerState) -> Self {
        self.scheduler_state = state;
        self
    }

    /// Supplies the RISC-V FP image and FS state saved by clone.
    #[cfg(target_arch = "riscv64")]
    pub fn with_fp_state(mut self, state: ax_cpu::registers::FpState) -> Self {
        self.fp_state = Some(state);
        self
    }

    /// Captures the current hardware FP owner during resource preparation.
    #[cfg(not(target_arch = "riscv64"))]
    pub fn inherit_current_fp(mut self) -> Self {
        self.fp_initialization = FpInitialization::InheritCurrent;
        self
    }
}

#[cfg(not(target_arch = "riscv64"))]
enum FpInitialization {
    Default,
    InheritCurrent,
}

fn prepare_user_thread_inner<F>(
    entry: F,
    thread: Thread,
    options: UserThreadOptions,
    address_space: ax_std::os::arceos::thread::TaskAddressSpace,
) -> Result<PreparedUserTask, scheduler::thread::TaskError>
where
    F: FnOnce() + Send + 'static,
{
    let name = options.name;
    let scheduler_tick_gate = thread.proc_data.scheduler_tick_gate();
    let scheduler_tick_cpu_time = thread.cpu_time().scheduler_tick_cpu_time();
    let irq_identity = IrqTaskIdentity::new(&thread, &name);
    let extension_name = prepare_task_name(&name)?;
    super::allocation::point()?;
    let data = Box::into_raw(
        Box::try_new(StarryUserTaskExtension {
            thread,
            name: Mutex::new(extension_name),
            irq_identity,
            reset_on_fork: AtomicBool::new(options.scheduler_state.reset_on_fork),
        })
        .map_err(|_| super::allocation::no_memory())?,
    ) as usize;
    // SAFETY: `data` is a uniquely owned `Box<StarryUserTaskExtension>`. The
    // runtime takes that ownership even when scheduler creation fails and
    // invokes `starry_user_task_drop` exactly once from task/reaper context.
    let extension = unsafe {
        scheduler::thread::ThreadExtension::new(data, &STARRY_USER_TASK_EXTENSION_OPS)
            .with_scheduler_tick_cpu_time(scheduler_tick_cpu_time)
            .with_running_policy_applied_hook(starry_user_task_policy_applied)
            .with_scheduler_tick_work(scheduler_tick_gate, starry_user_task_scheduler_tick)
    };
    let mut builder = kernel_thread_builder(name)
        .policy(options.scheduler_state.policy)
        .extension(extension);
    if let Some(affinity) = options.scheduler_state.affinity {
        builder = builder.affinity(affinity);
    }
    let runtime_options = ax_std::os::arceos::thread::UserContextOptions::new(address_space);
    #[cfg(target_arch = "riscv64")]
    let runtime_options = match options.fp_state {
        Some(fp) => runtime_options.with_fp_state(fp),
        None => runtime_options,
    };
    #[cfg(not(target_arch = "riscv64"))]
    let runtime_options = match options.fp_initialization {
        FpInitialization::InheritCurrent => runtime_options.inherit_current_fp(),
        FpInitialization::Default => runtime_options,
    };
    // SAFETY: the user entry and its uniquely owned MM/FP state belong to this task.
    let prepared = unsafe {
        ax_std::os::arceos::thread::prepare_user_thread(builder, entry, runtime_options)?
    };
    let scheduler_id = prepared.thread_handle().id();
    // SAFETY: `data` was created above for this scheduler extension, and the
    // prepared token still owns that extension until it is staged or dropped.
    let extension_data = unsafe { extension_data_from_raw(data) };
    extension_data
        .thread
        .bind_scheduler_id(scheduler_id)
        .map_err(|_| scheduler::thread::TaskError::InvalidRuntimeHandle)?;
    Ok(PreparedUserTask {
        scheduler: prepared,
        extension_data: data,
    })
}

fn prepare_task_name(name: &str) -> Result<Arc<String>, scheduler::thread::TaskError> {
    super::allocation::try_arc(super::allocation::try_string(name)?)
}

fn finish_published_user_thread(handle: scheduler::thread::ThreadHandle) -> UserTaskRef {
    match UserTaskRef::try_from_scheduler(handle) {
        Ok(Some(task)) => task,
        Ok(None) => panic!("published Starry user thread lost its user extension"),
        Err(error) => panic!("published Starry user thread has invalid identity: {error}"),
    }
}

struct StarryUserTaskExtension {
    thread: Thread,
    name: Mutex<Arc<String>>,
    irq_identity: IrqTaskIdentity,
    reset_on_fork: AtomicBool,
}

struct IrqTaskIdentity {
    thread_identity: PidSnapshot,
    process_identity: PidSnapshot,
    uses_process_identity: AtomicBool,
    comm_sequence: AtomicU32,
    comm: [AtomicU8; TASK_COMM_LEN],
}

impl IrqTaskIdentity {
    fn new(thread: &Thread, name: &str) -> Self {
        let identity = Self {
            thread_identity: thread.pid_identity().snapshot(),
            process_identity: thread.proc_data.identity().snapshot(),
            uses_process_identity: AtomicBool::new(false),
            comm_sequence: AtomicU32::new(0),
            comm: core::array::from_fn(|_| AtomicU8::new(0)),
        };
        identity.set_comm(name);
        identity
    }

    fn thread_identity(&self) -> &PidSnapshot {
        if self.uses_process_identity.load(Ordering::Acquire) {
            &self.process_identity
        } else {
            &self.thread_identity
        }
    }

    fn transfer_to_process_identity(&self, identity: &PidIdentity) -> crate::StarryResult<()> {
        if self.process_identity.identity_id() != identity.id() {
            return Err(crate::StarryError::BadState);
        }
        self.uses_process_identity.store(true, Ordering::Release);
        Ok(())
    }

    fn set_comm(&self, name: &str) {
        let sequence = self.comm_sequence.fetch_add(1, Ordering::AcqRel);
        debug_assert_eq!(sequence & 1, 0, "task comm writers must be serialized");
        let bytes = name.as_bytes();
        for (index, slot) in self.comm.iter().enumerate() {
            let byte = if index < TASK_COMM_LEN - 1 {
                bytes.get(index).copied().unwrap_or(0)
            } else {
                0
            };
            slot.store(byte, Ordering::Release);
        }
        self.comm_sequence
            .store(sequence.wrapping_add(2), Ordering::Release);
    }

    fn copy_comm(&self, output: &mut [u8; TASK_COMM_LEN]) -> Option<usize> {
        let before = self.comm_sequence.load(Ordering::Acquire);
        if before & 1 != 0 {
            return None;
        }
        let mut len = 0;
        for (source, destination) in self.comm.iter().zip(output.iter_mut()) {
            let byte = source.load(Ordering::Acquire);
            *destination = byte;
            if byte == 0 {
                break;
            }
            len += 1;
        }
        let after = self.comm_sequence.load(Ordering::Acquire);
        (before == after && after & 1 == 0).then_some(len)
    }
}

static STARRY_USER_TASK_EXTENSION_OPS: scheduler::thread::ThreadExtensionOps =
    scheduler::thread::ThreadExtensionOps {
        on_switch_in: starry_user_task_switch_in,
        on_switch_out: starry_user_task_switch_out,
        on_exit: starry_user_task_exit,
        on_deadline_overrun: starry_user_task_deadline_overrun,
        drop: starry_user_task_drop,
    };

unsafe extern "Rust" fn starry_user_task_policy_applied(
    data: usize,
    _thread: scheduler::thread::ThreadId,
    base_policy: scheduler::sched::SchedulePolicy,
    observed_ns: u64,
) {
    let extension = unsafe { extension_data_from_raw(data) };
    let realtime_policy = is_realtime_policy(base_policy);
    extension
        .thread
        .apply_cpu_time_policy(realtime_policy, observed_ns);
}

unsafe extern "Rust" fn starry_user_task_switch_in(
    data: usize,
    thread: scheduler::thread::ThreadId,
    base_policy: scheduler::sched::SchedulePolicy,
    charged_runtime_ns: u64,
) {
    let extension = unsafe { extension_data_from_raw(data) };
    // SAFETY: scheduler extension hooks run with local IRQs disabled from the
    // final switch baton, which pins this scoped callback to the owner CPU.
    unsafe {
        ax_runtime::hal::percpu::with_cpu_pin(|pin| {
            CURRENT_USER_EXTENSION.write_current(pin, data);
            extension.thread.scheduler_switch_in(
                thread,
                is_realtime_policy(base_policy),
                charged_runtime_ns,
                pin,
            );
        })
        .unwrap_or_else(|_| panic!("Starry switch-in has no bound per-CPU area"));
    }
}

unsafe extern "Rust" fn starry_user_task_switch_out(
    data: usize,
    _thread: scheduler::thread::ThreadId,
    reason: scheduler::thread::SwitchReason,
) {
    let extension = unsafe { extension_data_from_raw(data) };
    // SAFETY: scheduler extension hooks run with local IRQs disabled from the
    // final switch baton, which pins this scoped callback to the owner CPU.
    unsafe {
        ax_runtime::hal::percpu::with_cpu_pin(|pin| {
            extension.thread.scheduler_switch_out(reason, pin);
            let current = CURRENT_USER_EXTENSION.read_current(pin);
            if current != data {
                panic!("Starry switch-out does not own the current-user slot");
            }
            CURRENT_USER_EXTENSION.write_current(pin, 0);
        })
        .unwrap_or_else(|_| panic!("Starry switch-out has no bound per-CPU area"));
    }
}

unsafe extern "Rust" fn starry_user_task_exit(_data: usize, _thread: scheduler::thread::ThreadId) {
    // Normal Linux task exit already released these counters before fd
    // teardown. This idempotent scheduler-lifetime fence also covers a
    // prepared thread whose publication failed after perf inheritance.
    #[cfg(target_arch = "aarch64")]
    {
        let extension = unsafe { extension_data_from_raw(_data) };
        crate::perf::task::on_scheduler_task_exit(&extension.thread);
    }
}

unsafe extern "Rust" fn starry_user_task_deadline_overrun(
    data: usize,
    _thread: scheduler::thread::ThreadId,
) {
    let data = unsafe { extension_data_from_raw(data) };
    data.thread.publish_deadline_overrun();
}

unsafe extern "Rust" fn starry_user_task_scheduler_tick(
    data: usize,
    _thread: scheduler::thread::ThreadId,
    observed_ns: u64,
) -> scheduler::runtime::service::SchedulerTickWorkDisposition {
    let extension = unsafe { extension_data_from_raw(data) };
    extension.thread.sample_scheduler_tick_cpu_time(observed_ns);
    super::signal::queue_rttime_limit_signal_from_scheduler_tick(&extension.thread, observed_ns);
    super::poll_process_cpu_timers_from_scheduler_tick(&extension.thread.proc_data);
    scheduler::runtime::service::SchedulerTickWorkDisposition::Complete
}

unsafe extern "Rust" fn starry_user_task_drop(data: usize) {
    // SAFETY: ownership of this exact box was transferred to the scheduler
    // extension, whose final callback invokes this function once.
    drop(unsafe { Box::from_raw(data as *mut StarryUserTaskExtension) });
}

fn try_extension_data(
    scheduler: &scheduler::thread::ThreadHandle,
) -> Result<Option<usize>, scheduler::thread::TaskError> {
    let extension = scheduler.extension();
    let StarryExtensionKind::User = classify_starry_extension(
        extension.as_ref().map(|extension| extension.ops()),
        extension.as_ref().map_or(0, |extension| extension.data()),
    )?
    else {
        return Ok(None);
    };
    let Some(extension) = extension else {
        unreachable!("classified Starry extension must be present")
    };
    Ok(Some(extension.data()))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StarryExtensionKind {
    MissingOrForeign,
    User,
}

fn classify_starry_extension(
    ops: Option<&'static scheduler::thread::ThreadExtensionOps>,
    data: usize,
) -> Result<StarryExtensionKind, scheduler::thread::TaskError> {
    let Some(ops) = ops else {
        return Ok(StarryExtensionKind::MissingOrForeign);
    };
    if !is_starry_thread_extension(ops) {
        return Ok(StarryExtensionKind::MissingOrForeign);
    }
    if data == 0 || !data.is_multiple_of(core::mem::align_of::<StarryUserTaskExtension>()) {
        return Err(scheduler::thread::TaskError::InvalidRuntimeHandle);
    }
    Ok(StarryExtensionKind::User)
}

fn is_starry_thread_extension(ops: &'static scheduler::thread::ThreadExtensionOps) -> bool {
    ptr::eq(ops, &STARRY_USER_TASK_EXTENSION_OPS)
}

const fn is_realtime_policy(policy: scheduler::sched::SchedulePolicy) -> bool {
    matches!(
        policy,
        scheduler::sched::SchedulePolicy::Fifo { .. }
            | scheduler::sched::SchedulePolicy::RoundRobin { .. }
    )
}

unsafe fn extension_data_from_raw(data: usize) -> &'static StarryUserTaskExtension {
    // SAFETY: callers either validated `STARRY_USER_TASK_EXTENSION_OPS` or are
    // callbacks reached exclusively through that static table.
    unsafe { &*(data as *const StarryUserTaskExtension) }
}

#[cfg(all(test, not(axtest)))]
mod tests {
    use super::*;

    static FOREIGN_EXTENSION_OPS: scheduler::thread::ThreadExtensionOps =
        scheduler::thread::ThreadExtensionOps {
            on_switch_in: foreign_thread_switch_in,
            on_switch_out: foreign_thread_switch_out,
            on_exit: foreign_thread_hook,
            on_deadline_overrun: foreign_thread_hook,
            drop: foreign_thread_drop,
        };

    #[test]
    fn accepts_only_starry_extension_ops_identity() {
        assert!(is_starry_thread_extension(&STARRY_USER_TASK_EXTENSION_OPS));
        assert!(!is_starry_thread_extension(&FOREIGN_EXTENSION_OPS));
    }

    #[test]
    fn missing_and_foreign_extensions_are_not_user_tasks() {
        assert_eq!(
            classify_starry_extension(None, 0),
            Ok(StarryExtensionKind::MissingOrForeign)
        );
        assert_eq!(
            classify_starry_extension(Some(&FOREIGN_EXTENSION_OPS), usize::MAX),
            Ok(StarryExtensionKind::MissingOrForeign)
        );
    }

    #[test]
    fn matching_ops_reject_malformed_extension_data() {
        assert_eq!(
            classify_starry_extension(Some(&STARRY_USER_TASK_EXTENSION_OPS), 0),
            Err(scheduler::thread::TaskError::InvalidRuntimeHandle)
        );
        assert_eq!(
            classify_starry_extension(Some(&STARRY_USER_TASK_EXTENSION_OPS), 1),
            Err(scheduler::thread::TaskError::InvalidRuntimeHandle)
        );
    }

    #[test]
    fn weak_generation_reuse_is_not_upgraded() {
        assert!(matches!(
            resolve_weak_scheduler_handle(Err(scheduler::thread::TaskError::StaleThreadId)),
            Ok(None)
        ));
        assert!(matches!(
            resolve_weak_scheduler_handle(Err(scheduler::thread::TaskError::NotInitialized)),
            Err(scheduler::thread::TaskError::NotInitialized)
        ));
    }

    #[test]
    fn rttime_classification_includes_only_fifo_and_round_robin() {
        let priority = scheduler::sched::RtPriority::new(1).unwrap();
        assert!(is_realtime_policy(scheduler::sched::SchedulePolicy::fifo(
            priority
        )));
        assert!(is_realtime_policy(
            scheduler::sched::SchedulePolicy::round_robin(priority)
        ));
        assert!(!is_realtime_policy(
            scheduler::sched::SchedulePolicy::default()
        ));
        let deadline = scheduler::sched::DeadlinePolicy::new(
            1_000_000,
            2_000_000,
            3_000_000,
            scheduler::sched::DeadlineFlags::NONE,
        )
        .unwrap();
        assert!(!is_realtime_policy(
            scheduler::sched::SchedulePolicy::Deadline(deadline,)
        ));
    }

    unsafe extern "Rust" fn foreign_thread_hook(
        _data: usize,
        _thread: scheduler::thread::ThreadId,
    ) {
    }

    unsafe extern "Rust" fn foreign_thread_switch_in(
        _data: usize,
        _thread: scheduler::thread::ThreadId,
        _policy: scheduler::sched::SchedulePolicy,
        _charged_runtime_ns: u64,
    ) {
    }

    unsafe extern "Rust" fn foreign_thread_switch_out(
        _data: usize,
        _thread: scheduler::thread::ThreadId,
        _reason: scheduler::thread::SwitchReason,
    ) {
    }

    unsafe extern "Rust" fn foreign_thread_drop(_data: usize) {}
}

#[cfg(axtest)]
#[axtest::axtest]
fn task_name_allocation_failure_preserves_snapshot() {
    use scheduler::{
        runtime::RuntimeStatus,
        thread::{TaskError, ThreadAllocationProbe},
    };
    let probe = ThreadAllocationProbe::fail_at(0).unwrap();
    assert!(
        matches!(UserThreadOptions::new("unpublished-child"), Err(TaskError::RuntimeFailure(code))
            if code == RuntimeStatus::NoMemory as u32),
        "initial thread name allocation failure must return ENOMEM"
    );
    assert_eq!(probe.attempts(), 1);
    drop(probe);
    assert_eq!(
        UserThreadOptions::new("recovered-child").unwrap().name,
        "recovered-child"
    );
    let original = prepare_task_name("existing").unwrap();
    for failure in 0..2 {
        let probe = ThreadAllocationProbe::fail_at(failure).unwrap();
        assert!(
            matches!(prepare_task_name("replacement"), Err(TaskError::RuntimeFailure(code))
            if code == RuntimeStatus::NoMemory as u32),
            "name allocation failure must return ENOMEM"
        );
        drop(probe);
        assert_eq!(original.as_str(), "existing");
        assert_eq!(
            prepare_task_name("recovered").unwrap().as_str(),
            "recovered"
        );
    }
}

#[cfg(axtest)]
#[axtest::axtest]
fn unpublished_extension_allocation_releases_process() {
    use scheduler::{
        runtime::RuntimeStatus,
        thread::{TaskError, ThreadAllocationProbe},
    };

    use crate::task::{PidReservation, PidReservationKind, ROOT_PID_NS, Tgid, Tid};

    for failure in 0..4 {
        let reservation =
            PidReservation::reserve(&ROOT_PID_NS, PidReservationKind::ProcessLeader).unwrap();
        let identity = reservation.identity();
        let tid = identity.acquire_role::<Tid>().unwrap();
        let tgid = identity.acquire_role::<Tgid>().unwrap();
        let process = crate::task::new_test_process_data(identity.clone(), tgid);
        let retired_process = Arc::downgrade(&process);
        let thread = Thread::new(
            identity,
            tid,
            process,
            None,
            Default::default(),
            scope_local::Scope::new(),
        )
        .unwrap();
        let mm =
            ax_runtime::thread::TaskAddressSpace::new(ax_cpu::mmu::read_kernel_page_table(), ())
                .unwrap();
        let probe = ThreadAllocationProbe::fail_at(failure).unwrap();
        let result = (|| {
            let options = UserThreadOptions::new("extension-rollback")?;
            prepare_user_thread_inner(
                || panic!("failed extension must not execute"),
                thread,
                options,
                mm,
            )
        })();
        assert!(
            matches!(result, Err(TaskError::RuntimeFailure(code)) if code == RuntimeStatus::NoMemory as u32)
        );
        assert_eq!(probe.attempts(), failure + 1);
        drop(probe);
        assert!(
            retired_process.upgrade().is_none(),
            "unpublished extension retained its process"
        );
        drop(reservation);
    }
}
