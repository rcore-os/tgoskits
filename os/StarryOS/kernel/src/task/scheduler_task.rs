//! Starry ownership adapter for runtime-backed scheduler threads.

use alloc::{boxed::Box, string::String};
use core::{
    ptr,
    sync::atomic::{AtomicBool, Ordering},
    task::{Context, Poll},
    time::Duration,
};

use ax_kspin::SpinNoIrq;
use ax_std::os::arceos::task as scheduler;

use super::Thread;

/// Strong Starry task reference backed by a generation-checked scheduler handle.
#[derive(Clone, Debug)]
pub struct StarryTaskRef {
    scheduler: scheduler::ThreadHandle,
}

impl StarryTaskRef {
    /// Recovers a Starry task after validating the extension callback identity.
    pub fn from_scheduler(handle: scheduler::ThreadHandle) -> Result<Self, scheduler::TaskError> {
        let data = extension_data(&handle)?;
        data.thread
            .bind_scheduler_id(handle.id())
            .map_err(|_| scheduler::TaskError::InvalidRuntimeHandle)?;
        Ok(Self { scheduler: handle })
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
        extension_data(&self.scheduler)
            .unwrap_or_else(|_| panic!("scheduler thread lost its Starry extension"))
            .thread
            .as_ref()
    }

    /// Returns the validated Starry thread through the former optional shape.
    pub fn try_as_thread(&self) -> Option<&Thread> {
        Some(self.as_thread())
    }

    /// Returns the diagnostic task name retained by the Starry extension.
    pub fn name(&self) -> String {
        extension_data(&self.scheduler)
            .unwrap_or_else(|_| panic!("scheduler thread lost its Starry extension"))
            .name
            .lock()
            .clone()
    }

    /// Replaces the Linux-visible thread command name.
    pub fn set_name(&self, name: &str) {
        *extension_data(&self.scheduler)
            .unwrap_or_else(|_| panic!("scheduler thread lost its Starry extension"))
            .name
            .lock() = String::from(name);
    }

    /// Tests identity without relying on an allocator pointer address.
    pub fn ptr_eq(&self, other: &Self) -> bool {
        self.id() == other.id()
    }

    /// Returns whether Linux `RESET_ON_FORK` is active for this thread.
    pub fn reset_on_fork(&self) -> bool {
        extension_data(&self.scheduler)
            .unwrap_or_else(|_| panic!("scheduler thread lost its Starry extension"))
            .reset_on_fork
            .load(Ordering::Acquire)
    }

    /// Updates Linux `RESET_ON_FORK` metadata after policy validation.
    pub fn set_reset_on_fork(&self, reset: bool) {
        extension_data(&self.scheduler)
            .unwrap_or_else(|_| panic!("scheduler thread lost its Starry extension"))
            .reset_on_fork
            .store(reset, Ordering::Release);
    }

    /// Synchronizes Linux RT-class accounting after a scheduler policy update.
    pub(crate) fn set_accounting_policy(&self, policy: scheduler::SchedulePolicy) {
        let data = extension_data(&self.scheduler)
            .unwrap_or_else(|_| panic!("scheduler thread lost its Starry extension"));
        let realtime_policy = is_realtime_policy(policy);
        let previous = data.realtime_policy.swap(realtime_policy, Ordering::AcqRel);
        data.thread
            .cpu_time
            .set_realtime_policy(realtime_policy, previous && !realtime_policy);
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
    pub fn downgrade(&self) -> WeakStarryTaskRef {
        WeakStarryTaskRef {
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

    /// Sets the Starry-local interruption bit and directly wakes this thread.
    pub fn interrupt(&self) {
        self.as_thread().interrupted.store(true, Ordering::Release);
        let _result = self.wake_handle().wake();
    }

    /// Tests and consumes one pending interruption.
    pub fn poll_interrupt(&self, _context: &Context<'_>) -> Poll<()> {
        if self.as_thread().interrupted.swap(false, Ordering::AcqRel) {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }

    /// Tests whether an interruption remains pending.
    pub fn interrupted(&self) -> bool {
        self.as_thread().interrupted.load(Ordering::Acquire)
    }

    /// Clears a stale interruption before returning to userspace.
    pub fn clear_interrupt(&self) {
        self.as_thread().interrupted.store(false, Ordering::Release);
    }

    /// Waits for exit and reaps the scheduler-owned runtime resources.
    pub fn join(self) -> i32 {
        scheduler::join_thread(self.scheduler)
            .unwrap_or_else(|error| panic!("failed to join Starry task: {error}"))
    }

    /// Returns and clears a pending Deadline-overrun notification.
    pub fn take_deadline_overrun(&self) -> bool {
        extension_data(&self.scheduler)
            .unwrap_or_else(|_| panic!("scheduler thread lost its Starry extension"))
            .deadline_overrun
            .swap(false, Ordering::AcqRel)
    }
}

impl PartialEq for StarryTaskRef {
    fn eq(&self, other: &Self) -> bool {
        self.scheduler.id() == other.scheduler.id()
    }
}

impl Eq for StarryTaskRef {}

/// Non-owning Starry task reference that cannot alias a reused registry slot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WeakStarryTaskRef {
    scheduler_id: scheduler::ThreadId,
}

impl WeakStarryTaskRef {
    /// Upgrades the reference only while the same slot generation is live.
    pub fn upgrade(self) -> Option<StarryTaskRef> {
        scheduler::thread_handle(self.scheduler_id)
            .ok()
            .and_then(|handle| StarryTaskRef::from_scheduler(handle).ok())
    }
}

/// Returns the calling Starry task after validating its extension identity.
pub fn current_starry_task() -> Result<StarryTaskRef, scheduler::TaskError> {
    StarryTaskRef::from_scheduler(scheduler::current_thread_handle()?)
}

/// Returns the calling Starry task.
///
/// A scheduler thread without a Starry extension is a kernel/runtime worker and
/// must not enter a Starry syscall or process path.
#[track_caller]
pub fn current() -> StarryTaskRef {
    current_starry_task()
        .unwrap_or_else(|error| panic!("current scheduler thread is not a Starry task: {error}"))
}

/// Spawns a runtime worker with the default kernel stack size.
pub fn spawn_with_name<F>(entry: F, name: String) -> scheduler::ThreadHandle
where
    F: FnOnce() + Send + 'static,
{
    spawn_raw(entry, name, crate::config::KERNEL_STACK_SIZE)
}

/// Spawns a runtime worker with an explicit kernel stack size.
pub fn spawn_raw<F>(entry: F, name: String, stack_size: usize) -> scheduler::ThreadHandle
where
    F: FnOnce() + Send + 'static,
{
    scheduler::spawn_raw(entry, name, stack_size)
        .unwrap_or_else(|error| panic!("failed to spawn runtime worker: {error}"))
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
pub fn spawn_starry_user_thread<F>(
    entry: F,
    name: String,
    stack_size: usize,
    page_table_root: usize,
    thread: Box<Thread>,
) -> Result<StarryTaskRef, scheduler::TaskError>
where
    F: FnOnce() + Send + 'static,
{
    let address_space = scheduler::TaskAddressSpace::from_page_table_root(page_table_root)?;
    spawn_starry_thread_inner(
        entry,
        name,
        stack_size,
        thread,
        StarryContextState::user(address_space),
    )
}

/// Creates a Starry user thread with inherited Linux scheduling state.
#[cfg(not(target_arch = "riscv64"))]
pub fn spawn_starry_user_thread_with_policy<F>(
    entry: F,
    name: String,
    stack_size: usize,
    page_table_root: usize,
    thread: Box<Thread>,
    policy: scheduler::SchedulePolicy,
    reset_on_fork: bool,
) -> Result<StarryTaskRef, scheduler::TaskError>
where
    F: FnOnce() + Send + 'static,
{
    let address_space = scheduler::TaskAddressSpace::from_page_table_root(page_table_root)?;
    spawn_starry_thread_inner(
        entry,
        name,
        stack_size,
        thread,
        StarryContextState::user_with_policy(address_space, policy, reset_on_fork),
    )
}

/// Creates a RISC-V user thread with inherited FP and scheduling state.
#[cfg(target_arch = "riscv64")]
pub fn spawn_starry_user_thread_with_fp_state_and_policy<F>(
    entry: F,
    name: String,
    stack_size: usize,
    page_table_root: usize,
    fp_state: ax_cpu::FpState,
    thread: Box<Thread>,
    policy: scheduler::SchedulePolicy,
    reset_on_fork: bool,
) -> Result<StarryTaskRef, scheduler::TaskError>
where
    F: FnOnce() + Send + 'static,
{
    let address_space = scheduler::TaskAddressSpace::from_page_table_root(page_table_root)?;
    spawn_starry_thread_inner(
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

fn spawn_starry_thread_inner<F>(
    entry: F,
    name: String,
    stack_size: usize,
    thread: Box<Thread>,
    context_state: StarryContextState,
) -> Result<StarryTaskRef, scheduler::TaskError>
where
    F: FnOnce() + Send + 'static,
{
    let data = Box::into_raw(Box::new(StarryThreadExtension {
        thread,
        name: SpinNoIrq::new(name.clone()),
        deadline_overrun: AtomicBool::new(false),
        reset_on_fork: AtomicBool::new(context_state.reset_on_fork),
        realtime_policy: AtomicBool::new(is_realtime_policy(context_state.policy)),
    })) as usize;
    // SAFETY: `data` is a uniquely owned `Box<StarryThreadExtension>`. The
    // runtime takes that ownership even when scheduler creation fails and
    // invokes `starry_thread_drop` exactly once from task/reaper context.
    let extension = unsafe { scheduler::ThreadExtension::new(data, &STARRY_THREAD_EXTENSION_OPS) };
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
    Ok(StarryTaskRef::from_scheduler(handle)
        .unwrap_or_else(|_| panic!("runtime lost a newly installed Starry extension")))
}

struct StarryThreadExtension {
    thread: Box<Thread>,
    name: SpinNoIrq<String>,
    deadline_overrun: AtomicBool,
    reset_on_fork: AtomicBool,
    realtime_policy: AtomicBool,
}

static STARRY_THREAD_EXTENSION_OPS: scheduler::ThreadExtensionOps = scheduler::ThreadExtensionOps {
    on_switch_in: starry_thread_switch_in,
    on_switch_out: starry_thread_switch_out,
    on_exit: starry_thread_exit,
    on_deadline_overrun: starry_thread_deadline_overrun,
    drop: starry_thread_drop,
};

unsafe extern "Rust" fn starry_thread_switch_in(data: usize, thread: scheduler::ThreadId) {
    let data = unsafe { extension_data_from_raw(data) };
    data.thread
        .scheduler_switch_in(thread, data.realtime_policy.load(Ordering::Acquire));
}

unsafe extern "Rust" fn starry_thread_switch_out(
    data: usize,
    _thread: scheduler::ThreadId,
    reason: scheduler::SwitchReason,
) {
    let data = unsafe { extension_data_from_raw(data) };
    data.thread.scheduler_switch_out(reason);
}

unsafe extern "Rust" fn starry_thread_exit(_data: usize, _thread: scheduler::ThreadId) {}

unsafe extern "Rust" fn starry_thread_deadline_overrun(data: usize, _thread: scheduler::ThreadId) {
    let data = unsafe { extension_data_from_raw(data) };
    data.deadline_overrun.store(true, Ordering::Release);
}

unsafe extern "Rust" fn starry_thread_drop(data: usize) {
    // SAFETY: ownership of this exact box was transferred to the runtime outer
    // extension, whose final callback invokes this function once.
    drop(unsafe { Box::from_raw(data as *mut StarryThreadExtension) });
}

fn extension_data(
    scheduler: &scheduler::ThreadHandle,
) -> Result<&StarryThreadExtension, scheduler::TaskError> {
    let extension = scheduler::thread_os_extension(scheduler)?
        .ok_or(scheduler::TaskError::InvalidRuntimeHandle)?;
    if !is_starry_thread_extension(extension.ops()) {
        return Err(scheduler::TaskError::InvalidRuntimeHandle);
    }
    // SAFETY: the checked callback-table identity is unique to
    // `StarryThreadExtension`. The returned borrow is tied to the strong
    // scheduler handle, which prevents the runtime record from being reaped.
    Ok(unsafe { extension_data_from_raw(extension.data()) })
}

fn is_starry_thread_extension(ops: &'static scheduler::ThreadExtensionOps) -> bool {
    ptr::eq(ops, &STARRY_THREAD_EXTENSION_OPS)
}

const fn is_realtime_policy(policy: scheduler::SchedulePolicy) -> bool {
    matches!(
        policy,
        scheduler::SchedulePolicy::Fifo { .. } | scheduler::SchedulePolicy::RoundRobin { .. }
    )
}

unsafe fn extension_data_from_raw(data: usize) -> &'static StarryThreadExtension {
    // SAFETY: callers either validated `STARRY_THREAD_EXTENSION_OPS` or are
    // callbacks reached exclusively through that static table.
    unsafe { &*(data as *const StarryThreadExtension) }
}

#[cfg(test)]
mod tests {
    use super::*;

    static FOREIGN_EXTENSION_OPS: scheduler::ThreadExtensionOps = scheduler::ThreadExtensionOps {
        on_switch_in: foreign_thread_hook,
        on_switch_out: foreign_thread_switch_out,
        on_exit: foreign_thread_hook,
        on_deadline_overrun: foreign_thread_hook,
        drop: foreign_thread_drop,
    };

    #[test]
    fn accepts_only_starry_extension_ops_identity() {
        assert!(is_starry_thread_extension(&STARRY_THREAD_EXTENSION_OPS));
        assert!(!is_starry_thread_extension(&FOREIGN_EXTENSION_OPS));
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

    unsafe extern "Rust" fn foreign_thread_drop(_data: usize) {}
}
