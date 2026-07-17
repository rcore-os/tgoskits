//! Starry ownership adapter for runtime-backed scheduler threads.

use alloc::{boxed::Box, string::String};
use core::{
    ptr,
    sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering},
    task::{Context, Poll},
    time::Duration,
};

use ax_kspin::{IrqGuard, SpinNoIrq};
use ax_runtime::task::{UserEntryAck, UserEntryTicket};
use ax_std::os::arceos::task as scheduler;

use super::Thread;

const TASK_COMM_LEN: usize = 16;

#[ax_percpu::def_percpu]
static CURRENT_USER_EXTENSION: usize = 0;

static CURRENT_USER_VIEW_FAILURES: AtomicU64 = AtomicU64::new(0);

/// Strong Starry user-task reference backed by a checked scheduler handle.
#[derive(Clone, Debug)]
pub struct UserTaskRef {
    scheduler: scheduler::ThreadHandle,
    extension_data: usize,
}

/// Result of acknowledging one bounded Starry user-return work pass.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use = "a retry must drain newly published Starry user-return work"]
pub(crate) enum UserReturnDecision {
    /// No newer work was published during the pass.
    Ready,
    /// A producer raced the pass, so signals and timers must be checked again.
    Retry,
}

impl UserTaskRef {
    /// Tries to recover a Starry user task from a generic scheduler thread.
    ///
    /// Threads without an inner OS extension or with a foreign inner extension
    /// return `Ok(None)`. A foreign runtime outer extension is a configuration
    /// error; matching Starry operations with malformed data are a runtime-
    /// handle error.
    pub fn try_from_scheduler(
        handle: scheduler::ThreadHandle,
    ) -> Result<Option<Self>, scheduler::TaskError> {
        let Some(extension_data) = try_extension_data(&handle)? else {
            return Ok(None);
        };
        // SAFETY: `try_extension_data` validated the callback-table identity,
        // pointer alignment, and non-null value while `handle` pins the outer
        // runtime extension. The handle is retained by the returned adapter.
        let data = unsafe { extension_data_from_raw(extension_data) };
        data.thread
            .bind_scheduler_id(handle.id())
            .map_err(|_| scheduler::TaskError::InvalidRuntimeHandle)?;
        Ok(Some(Self {
            scheduler: handle,
            extension_data,
        }))
    }

    /// Returns the generation-bearing scheduler identity.
    pub fn id(&self) -> scheduler::ThreadId {
        self.scheduler.id()
    }

    /// Formats the scheduler identity and diagnostic name.
    pub fn id_name(&self) -> String {
        alloc::format!("Task({}, {:?})", self.id().as_u64(), self.name())
    }

    /// Returns the Starry thread attached through the checked extension.
    pub fn as_thread(&self) -> &Thread {
        self.extension().thread.as_ref()
    }

    /// Returns the diagnostic task name retained by the Starry extension.
    pub fn name(&self) -> String {
        self.extension().name.lock().clone()
    }

    /// Replaces the Linux-visible thread command name.
    pub fn set_name(&self, name: &str) {
        let extension = self.extension();
        let mut stored_name = extension.name.lock();
        *stored_name = String::from(name);
        extension.irq_identity.set_comm(name);
    }

    /// Tests identity without relying on an allocator pointer address.
    pub fn ptr_eq(&self, other: &Self) -> bool {
        self.id() == other.id()
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

    /// Commits an exec-time page-table replacement for the running thread.
    pub fn switch_page_table(&self, root: ax_memory_addr::PhysAddr) {
        assert_eq!(
            self.id(),
            scheduler::current_thread_id()
                .unwrap_or_else(|error| panic!("page-table switch has no current task: {error}")),
            "only the running task may replace its page table"
        );
        ax_runtime::task::switch_current_page_table(root.as_usize())
            .unwrap_or_else(|error| panic!("failed to replace current page table: {error}"));
    }

    /// Creates a non-owning generation-checked task reference.
    pub fn downgrade(&self) -> WeakUserTaskRef {
        WeakUserTaskRef {
            scheduler_id: self.scheduler.id(),
        }
    }

    /// Creates a stable direct-wake handle for IRQ or remote producers.
    pub fn wake_handle(&self) -> scheduler::ThreadWakeHandle {
        self.scheduler.wake_handle()
    }

    /// Returns the scheduler lifecycle snapshot.
    pub fn state(&self) -> scheduler::ThreadState {
        self.scheduler.state()
    }

    /// Returns the last CPU selected for this task, if placement is known.
    pub fn cpu_id(&self) -> usize {
        self.wake_handle()
            .target_cpu()
            .map_or(0, |cpu| cpu.as_u32() as usize)
    }

    /// Returns the base scheduling policy.
    pub fn policy(&self) -> scheduler::SchedulePolicy {
        self.scheduler.policy()
    }

    /// Returns the scheduler affinity snapshot.
    pub fn affinity(&self) -> scheduler::CpuSet {
        scheduler::thread_affinity(self.id())
            .unwrap_or_else(|error| panic!("failed to read Starry task affinity: {error}"))
    }

    /// Publishes Starry user-entry work and directly wakes this thread.
    pub fn interrupt(&self) {
        self.as_thread().user_entry_notification.publish();
        let _result = self.wake_handle().wake();
    }

    /// Tests one pending interruption without acknowledging user-return work.
    pub fn poll_interrupt(&self, _context: &Context<'_>) -> Poll<()> {
        if self.interruption_pending() {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }

    /// Tests whether unacknowledged user-entry work remains pending.
    pub fn interruption_pending(&self) -> bool {
        self.as_thread().user_entry_notification.pending()
    }

    /// Captures a baseline for a wait that must ignore older notifications.
    pub(crate) fn interrupt_snapshot(&self) -> UserEntryTicket<'_> {
        self.as_thread().user_entry_notification.snapshot()
    }

    /// Tests whether an interruption was published after `snapshot`.
    pub(crate) fn interrupted_since(&self, snapshot: &UserEntryTicket<'_>) -> bool {
        self.as_thread()
            .user_entry_notification
            .changed_since(snapshot)
    }

    /// Captures the newest notification before draining exit-to-user work.
    pub(crate) fn begin_user_return_work(&self) -> UserEntryTicket<'_> {
        self.as_thread().user_entry_notification.snapshot()
    }

    /// Acknowledges only the captured work and reports a concurrent producer.
    pub(crate) fn finish_user_return_work(
        &self,
        snapshot: UserEntryTicket<'_>,
    ) -> UserReturnDecision {
        match self
            .as_thread()
            .user_entry_notification
            .acknowledge(snapshot)
        {
            UserEntryAck::Stable => UserReturnDecision::Ready,
            UserEntryAck::Pending => UserReturnDecision::Retry,
        }
    }

    /// Returns the concrete notification checked by ax-runtime with IRQs off.
    pub(crate) fn user_entry_notification(&self) -> &ax_runtime::task::UserEntryNotification {
        &self.as_thread().user_entry_notification
    }

    /// Waits for exit and reaps the scheduler-owned runtime resources.
    pub fn join(self) -> i32 {
        scheduler::join_thread(self.scheduler)
            .unwrap_or_else(|error| panic!("failed to join Starry task: {error}"))
    }

    /// Returns and clears a pending Deadline-overrun notification.
    pub fn take_deadline_overrun(&self) -> bool {
        self.extension()
            .deadline_overrun
            .swap(false, Ordering::AcqRel)
    }

    fn extension(&self) -> &StarryUserTaskExtension {
        // SAFETY: construction validates this value and retains the scheduler
        // handle that owns the enclosing runtime extension for `self`'s whole
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
    scheduler_id: scheduler::ThreadId,
}

impl WeakUserTaskRef {
    /// Upgrades the reference only while the same slot generation is live.
    pub fn upgrade(self) -> Result<Option<UserTaskRef>, scheduler::TaskError> {
        let Some(handle) =
            resolve_weak_scheduler_handle(scheduler::thread_handle(self.scheduler_id))?
        else {
            return Ok(None);
        };
        UserTaskRef::try_from_scheduler(handle)
    }
}

fn resolve_weak_scheduler_handle(
    lookup: Result<scheduler::ThreadHandle, scheduler::TaskError>,
) -> Result<Option<scheduler::ThreadHandle>, scheduler::TaskError> {
    match lookup {
        Ok(handle) => Ok(Some(handle)),
        Err(scheduler::TaskError::StaleThreadId) => Ok(None),
        Err(error) => Err(error),
    }
}

/// Tries to recover a Starry user task for the calling scheduler thread.
pub fn try_current_user_task() -> Result<Option<UserTaskRef>, scheduler::TaskError> {
    UserTaskRef::try_from_scheduler(scheduler::current_thread_handle()?)
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
    _irq_guard: IrqGuard,
}

impl UserTaskIrqView {
    /// Returns the Linux thread ID cached before the task became runnable.
    pub(crate) fn tid(&self) -> u32 {
        self.extension().thread.tid()
    }

    /// Returns the Linux thread-group ID cached before the task became runnable.
    pub(crate) fn tgid(&self) -> u32 {
        self.extension().irq_identity.tgid
    }

    /// Copies the lock-free Linux command-name snapshot into `output`.
    pub(crate) fn copy_comm(&self, output: &mut [u8; TASK_COMM_LEN]) -> Option<usize> {
        self.extension().irq_identity.copy_comm(output)
    }

    /// Pushes one return-probe instance without allocation or recursive spin.
    pub(crate) fn push_kretprobe(&self, instance: kprobe::retprobe::RetprobeInstance) {
        let Some(mut stack) = self.extension().thread.kretprobe_stack.try_lock() else {
            panic!("nested kretprobe tried to re-enter the current task stack");
        };
        if stack.len() == super::KRETPROBE_STACK_CAPACITY {
            core::mem::forget(instance);
            panic!("current task exceeded its fixed kretprobe nesting capacity");
        }
        stack.push(instance);
    }

    /// Pops one return-probe instance without allocation or recursive spin.
    pub(crate) fn pop_kretprobe(&self) -> kprobe::retprobe::RetprobeInstance {
        let Some(mut stack) = self.extension().thread.kretprobe_stack.try_lock() else {
            panic!("nested kretprobe tried to re-enter the current task stack");
        };
        stack.pop().expect("kretprobe instance stack underflow")
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
    let irq_guard = IrqGuard::new();
    let bound = match ax_percpu::bound_current(irq_guard.cpu_pin()) {
        Ok(bound) => bound,
        Err(_) => {
            CURRENT_USER_VIEW_FAILURES.fetch_add(1, Ordering::Relaxed);
            return None;
        }
    };
    let extension_data = CURRENT_USER_EXTENSION.read_current(&bound);
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

/// Spawns a kernel worker without installing a Starry user-task extension.
pub fn spawn_kernel_thread<F>(entry: F, name: String) -> scheduler::ThreadHandle
where
    F: FnOnce() + Send + 'static,
{
    try_spawn_kernel_thread(entry, name)
        .unwrap_or_else(|error| panic!("failed to spawn kernel thread: {error}"))
}

/// Spawns a kernel worker with an explicit kernel stack size.
pub fn spawn_kernel_thread_with_stack<F>(
    entry: F,
    name: String,
    stack_size: usize,
) -> scheduler::ThreadHandle
where
    F: FnOnce() + Send + 'static,
{
    try_spawn_kernel_thread_with_stack(entry, name, stack_size)
        .unwrap_or_else(|error| panic!("failed to spawn kernel thread: {error}"))
}

/// Tries to spawn a kernel worker with Starry's default kernel stack size.
pub fn try_spawn_kernel_thread<F>(
    entry: F,
    name: String,
) -> Result<scheduler::ThreadHandle, scheduler::TaskError>
where
    F: FnOnce() + Send + 'static,
{
    try_spawn_kernel_thread_with_stack(entry, name, crate::config::KERNEL_STACK_SIZE)
}

/// Tries to spawn a kernel worker without installing a user-task extension.
pub fn try_spawn_kernel_thread_with_stack<F>(
    entry: F,
    name: String,
    stack_size: usize,
) -> Result<scheduler::ThreadHandle, scheduler::TaskError>
where
    F: FnOnce() + Send + 'static,
{
    scheduler::spawn_raw(entry, name, stack_size)
}

/// Returns Starry's default kernel stack size.
pub const fn default_task_stack_size() -> usize {
    crate::config::KERNEL_STACK_SIZE
}

/// Yields the calling scheduler thread.
pub fn yield_now() {
    scheduler::yield_current_cpu()
        .unwrap_or_else(|error| panic!("failed to yield current scheduler thread: {error}"));
}

/// Sleeps the calling scheduler thread for at least `duration`.
pub fn sleep(duration: Duration) {
    scheduler::sleep(duration);
}

/// Diagnoses an invalid attempt to sleep from hard-IRQ context.
#[track_caller]
pub fn might_sleep() {
    assert!(
        !ax_runtime::hal::irq::in_irq_context(),
        "sleeping operation entered from hard IRQ context"
    );
}

/// Creates and enqueues a Starry user thread bound to one page-table root.
pub fn spawn_user_thread<F>(
    entry: F,
    name: String,
    stack_size: usize,
    page_table_root: usize,
    thread: Box<Thread>,
) -> Result<UserTaskRef, scheduler::TaskError>
where
    F: FnOnce() + Send + 'static,
{
    let address_space = scheduler::TaskAddressSpace::from_page_table_root(page_table_root)?;
    spawn_user_thread_inner(
        entry,
        name,
        stack_size,
        thread,
        StarryContextState::user(address_space),
    )
}

/// Creates a Starry user thread with inherited Linux scheduling state.
#[cfg(not(target_arch = "riscv64"))]
pub fn spawn_user_thread_with_policy<F>(
    entry: F,
    name: String,
    stack_size: usize,
    page_table_root: usize,
    thread: Box<Thread>,
    policy: scheduler::SchedulePolicy,
    reset_on_fork: bool,
) -> Result<UserTaskRef, scheduler::TaskError>
where
    F: FnOnce() + Send + 'static,
{
    let address_space = scheduler::TaskAddressSpace::from_page_table_root(page_table_root)?;
    spawn_user_thread_inner(
        entry,
        name,
        stack_size,
        thread,
        StarryContextState::user_with_policy(address_space, policy, reset_on_fork),
    )
}

/// Creates a RISC-V user thread with inherited FP and scheduling state.
#[cfg(target_arch = "riscv64")]
pub fn spawn_user_thread_with_fp_state_and_policy<F>(
    entry: F,
    name: String,
    stack_size: usize,
    page_table_root: usize,
    fp_state: ax_cpu::FpState,
    thread: Box<Thread>,
    policy: scheduler::SchedulePolicy,
    reset_on_fork: bool,
) -> Result<UserTaskRef, scheduler::TaskError>
where
    F: FnOnce() + Send + 'static,
{
    let address_space = scheduler::TaskAddressSpace::from_page_table_root(page_table_root)?;
    spawn_user_thread_inner(
        entry,
        name,
        stack_size,
        thread,
        StarryContextState {
            address_space: Some(address_space),
            fp_state: Some(fp_state),
            policy,
            reset_on_fork,
        },
    )
}

struct StarryContextState {
    address_space: Option<scheduler::TaskAddressSpace>,
    #[cfg(target_arch = "riscv64")]
    fp_state: Option<ax_cpu::FpState>,
    policy: scheduler::SchedulePolicy,
    reset_on_fork: bool,
}

impl StarryContextState {
    fn user(address_space: scheduler::TaskAddressSpace) -> Self {
        Self {
            address_space: Some(address_space),
            #[cfg(target_arch = "riscv64")]
            fp_state: None,
            policy: scheduler::SchedulePolicy::default(),
            reset_on_fork: false,
        }
    }

    #[cfg(not(target_arch = "riscv64"))]
    fn user_with_policy(
        address_space: scheduler::TaskAddressSpace,
        policy: scheduler::SchedulePolicy,
        reset_on_fork: bool,
    ) -> Self {
        Self {
            address_space: Some(address_space),
            #[cfg(target_arch = "riscv64")]
            fp_state: None,
            policy,
            reset_on_fork,
        }
    }
}

fn spawn_user_thread_inner<F>(
    entry: F,
    name: String,
    stack_size: usize,
    thread: Box<Thread>,
    context_state: StarryContextState,
) -> Result<UserTaskRef, scheduler::TaskError>
where
    F: FnOnce() + Send + 'static,
{
    let irq_identity = IrqTaskIdentity::new(&thread, &name);
    let data = Box::into_raw(Box::new(StarryUserTaskExtension {
        thread,
        name: SpinNoIrq::new(name.clone()),
        irq_identity,
        deadline_overrun: AtomicBool::new(false),
        reset_on_fork: AtomicBool::new(context_state.reset_on_fork),
        realtime_policy: AtomicBool::new(is_realtime_policy(context_state.policy)),
    })) as usize;
    // SAFETY: `data` is a uniquely owned `Box<StarryUserTaskExtension>`. The
    // runtime takes that ownership even when scheduler creation fails and
    // invokes `starry_user_task_drop` exactly once from task/reaper context.
    let extension =
        unsafe { scheduler::ThreadExtension::new(data, &STARRY_USER_TASK_EXTENSION_OPS) };
    // SAFETY: the extension above transfers its unique callback-data ownership
    // to the runtime and is never used or dropped again by this function.
    let handle = unsafe {
        match context_state.address_space {
            #[cfg(target_arch = "riscv64")]
            Some(address_space) if context_state.fp_state.is_some() => {
                scheduler::spawn_raw_with_extension_in_address_space_and_fp_state_and_policy(
                    entry,
                    name,
                    stack_size,
                    Some(extension),
                    address_space,
                    context_state
                        .fp_state
                        .unwrap_or_else(|| unreachable!("guard checked FP state")),
                    context_state.policy,
                )?
            }
            Some(address_space) => scheduler::spawn_raw_with_extension_in_address_space_and_policy(
                entry,
                name,
                stack_size,
                Some(extension),
                address_space,
                context_state.policy,
            )?,
            None => scheduler::spawn_raw_with_extension(entry, name, stack_size, Some(extension))?,
        }
    };
    Ok(finish_published_user_thread(handle))
}

fn finish_published_user_thread(handle: scheduler::ThreadHandle) -> UserTaskRef {
    match UserTaskRef::try_from_scheduler(handle) {
        Ok(Some(task)) => task,
        Ok(None) => panic!("published Starry user thread lost its user extension"),
        Err(error) => panic!("published Starry user thread has invalid identity: {error}"),
    }
}

struct StarryUserTaskExtension {
    thread: Box<Thread>,
    name: SpinNoIrq<String>,
    irq_identity: IrqTaskIdentity,
    deadline_overrun: AtomicBool,
    reset_on_fork: AtomicBool,
    realtime_policy: AtomicBool,
}

struct IrqTaskIdentity {
    tgid: u32,
    comm_sequence: AtomicU32,
    comm: [AtomicU8; TASK_COMM_LEN],
}

impl IrqTaskIdentity {
    fn new(thread: &Thread, name: &str) -> Self {
        let identity = Self {
            tgid: thread.proc_data.proc.pid(),
            comm_sequence: AtomicU32::new(0),
            comm: core::array::from_fn(|_| AtomicU8::new(0)),
        };
        identity.set_comm(name);
        identity
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

static STARRY_USER_TASK_EXTENSION_OPS: scheduler::ThreadExtensionOps =
    scheduler::ThreadExtensionOps {
        on_switch_in: starry_user_task_switch_in,
        on_switch_out: starry_user_task_switch_out,
        on_policy_applied: starry_user_task_policy_applied,
        on_exit: starry_user_task_exit,
        on_deadline_overrun: starry_user_task_deadline_overrun,
        drop: starry_user_task_drop,
    };

unsafe extern "Rust" fn starry_user_task_switch_in(data: usize, thread: scheduler::ThreadId) {
    let extension = unsafe { extension_data_from_raw(data) };
    // SAFETY: scheduler extension hooks run with local IRQs disabled from the
    // final switch baton, which pins this callback to the owner CPU.
    let cpu_pin = unsafe { ax_cpu_local::CpuPin::new_unchecked() };
    let bound = ax_percpu::bound_current(&cpu_pin)
        .unwrap_or_else(|_| panic!("Starry switch-in has no bound per-CPU area"));
    CURRENT_USER_EXTENSION.write_current(&bound, data);
    extension.thread.scheduler_switch_in(
        thread,
        extension.realtime_policy.load(Ordering::Acquire),
        &cpu_pin,
    );
}

unsafe extern "Rust" fn starry_user_task_switch_out(
    data: usize,
    _thread: scheduler::ThreadId,
    reason: scheduler::SwitchReason,
) {
    let extension = unsafe { extension_data_from_raw(data) };
    // SAFETY: scheduler extension hooks run with local IRQs disabled from the
    // final switch baton, which pins this callback to the owner CPU.
    let cpu_pin = unsafe { ax_cpu_local::CpuPin::new_unchecked() };
    extension.thread.scheduler_switch_out(reason, &cpu_pin);
    let bound = ax_percpu::bound_current(&cpu_pin)
        .unwrap_or_else(|_| panic!("Starry switch-out has no bound per-CPU area"));
    let current = CURRENT_USER_EXTENSION.read_current(&bound);
    if current != data {
        panic!("Starry switch-out does not own the current-user slot");
    }
    CURRENT_USER_EXTENSION.write_current(&bound, 0);
}

unsafe extern "Rust" fn starry_user_task_policy_applied(
    data: usize,
    _thread: scheduler::ThreadId,
    event: scheduler::ThreadPolicyApplied,
) {
    let extension = unsafe { extension_data_from_raw(data) };
    let previous_realtime = event.previous_class().is_realtime();
    let current_realtime = event.current_class().is_realtime();
    assert_eq!(
        extension.realtime_policy.load(Ordering::Acquire),
        previous_realtime,
        "Starry accounting policy diverged from the applied scheduler generation"
    );
    extension.thread.cpu_time.set_realtime_policy_at(
        current_realtime,
        previous_realtime && !current_realtime,
        event.now_ns(),
    );
    extension
        .realtime_policy
        .store(current_realtime, Ordering::Release);
}

unsafe extern "Rust" fn starry_user_task_exit(_data: usize, _thread: scheduler::ThreadId) {}

unsafe extern "Rust" fn starry_user_task_deadline_overrun(
    data: usize,
    _thread: scheduler::ThreadId,
) {
    let data = unsafe { extension_data_from_raw(data) };
    data.deadline_overrun.store(true, Ordering::Release);
}

unsafe extern "Rust" fn starry_user_task_drop(data: usize) {
    // SAFETY: ownership of this exact box was transferred to the runtime outer
    // extension, whose final callback invokes this function once.
    drop(unsafe { Box::from_raw(data as *mut StarryUserTaskExtension) });
}

fn try_extension_data(
    scheduler: &scheduler::ThreadHandle,
) -> Result<Option<usize>, scheduler::TaskError> {
    let extension = scheduler::thread_os_extension(scheduler)?;
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
    ops: Option<&'static scheduler::ThreadExtensionOps>,
    data: usize,
) -> Result<StarryExtensionKind, scheduler::TaskError> {
    let Some(ops) = ops else {
        return Ok(StarryExtensionKind::MissingOrForeign);
    };
    if !is_starry_thread_extension(ops) {
        return Ok(StarryExtensionKind::MissingOrForeign);
    }
    if data == 0 || !data.is_multiple_of(core::mem::align_of::<StarryUserTaskExtension>()) {
        return Err(scheduler::TaskError::InvalidRuntimeHandle);
    }
    Ok(StarryExtensionKind::User)
}

fn is_starry_thread_extension(ops: &'static scheduler::ThreadExtensionOps) -> bool {
    ptr::eq(ops, &STARRY_USER_TASK_EXTENSION_OPS)
}

const fn is_realtime_policy(policy: scheduler::SchedulePolicy) -> bool {
    matches!(
        policy,
        scheduler::SchedulePolicy::Fifo { .. } | scheduler::SchedulePolicy::RoundRobin { .. }
    )
}

unsafe fn extension_data_from_raw(data: usize) -> &'static StarryUserTaskExtension {
    // SAFETY: callers either validated `STARRY_USER_TASK_EXTENSION_OPS` or are
    // callbacks reached exclusively through that static table.
    unsafe { &*(data as *const StarryUserTaskExtension) }
}

#[cfg(test)]
mod tests {
    use super::*;

    static FOREIGN_EXTENSION_OPS: scheduler::ThreadExtensionOps = scheduler::ThreadExtensionOps {
        on_switch_in: foreign_thread_hook,
        on_switch_out: foreign_thread_switch_out,
        on_policy_applied: foreign_thread_policy_applied,
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
            Err(scheduler::TaskError::InvalidRuntimeHandle)
        );
        assert_eq!(
            classify_starry_extension(Some(&STARRY_USER_TASK_EXTENSION_OPS), 1),
            Err(scheduler::TaskError::InvalidRuntimeHandle)
        );
    }

    #[test]
    fn weak_generation_reuse_is_not_upgraded() {
        assert!(matches!(
            resolve_weak_scheduler_handle(Err(scheduler::TaskError::StaleThreadId)),
            Ok(None)
        ));
        assert!(matches!(
            resolve_weak_scheduler_handle(Err(scheduler::TaskError::NotInitialized)),
            Err(scheduler::TaskError::NotInitialized)
        ));
    }

    #[test]
    fn rttime_classification_includes_only_fifo_and_round_robin() {
        let priority = scheduler::RtPriority::new(1).unwrap();
        assert!(is_realtime_policy(scheduler::SchedulePolicy::fifo(
            priority
        )));
        assert!(is_realtime_policy(scheduler::SchedulePolicy::round_robin(
            priority
        )));
        assert!(!is_realtime_policy(scheduler::SchedulePolicy::default()));
        let deadline = scheduler::DeadlinePolicy::new(
            1_000_000,
            2_000_000,
            3_000_000,
            scheduler::DeadlineFlags::NONE,
        )
        .unwrap();
        assert!(!is_realtime_policy(scheduler::SchedulePolicy::Deadline(
            deadline,
        )));
    }

    unsafe extern "Rust" fn foreign_thread_hook(_data: usize, _thread: scheduler::ThreadId) {}

    unsafe extern "Rust" fn foreign_thread_switch_out(
        _data: usize,
        _thread: scheduler::ThreadId,
        _reason: scheduler::SwitchReason,
    ) {
    }

    unsafe extern "Rust" fn foreign_thread_policy_applied(
        _data: usize,
        _thread: scheduler::ThreadId,
        _event: scheduler::ThreadPolicyApplied,
    ) {
    }

    unsafe extern "Rust" fn foreign_thread_drop(_data: usize) {}
}
