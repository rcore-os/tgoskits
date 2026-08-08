// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Axvisor realtime CPU partitioning and secondary CPU entry.
//!
//! The realtime CPU runs a private cooperative executor after `ax-runtime`
//! diverts it away from the ordinary host scheduler path.

use core::{
    cell::UnsafeCell,
    mem::MaybeUninit,
    pin::Pin,
    ptr::NonNull,
    sync::atomic::{AtomicU8, AtomicU64, AtomicUsize, Ordering},
};

use ax_std::os::arceos::modules::ax_hal::{
    context::{KernelTlsBase, TaskContext},
    percpu::{CurrentContext, CurrentThreadHeader, PreviousThreadBinding},
};

const HEARTBEAT_INTERVAL_NANOS: u64 = 1_000_000;
const WATCHDOG_INTERVAL_NANOS: u64 = 100_000_000;
const HEARTBEAT_TASK: usize = 0;
const WATCHDOG_TASK: usize = 1;
const HELLO_TASK: usize = 2;
const RT_TASK_COUNT: usize = 3;
const RT_STACK_SIZE: usize = 64 * 1024;
const RT_STACK_ALIGN: usize = 16;
const EXECUTOR_CONTEXT_ID: usize = 1;
const HELLO_INTERVAL_NANOS: u64 = 1_000_000_000;
const HELLO_RUNS: u64 = 5;
const RT_OUTPUT_CAPACITY: usize = 1024;

static RT_CPU_ID: AtomicUsize = AtomicUsize::new(usize::MAX);
static RT_STATE: AtomicUsize = AtomicUsize::new(RtState::Offline as usize);
static RT_HEARTBEATS: AtomicU64 = AtomicU64::new(0);
static RT_WATCHDOG_RUNS: AtomicU64 = AtomicU64::new(0);
static RT_ENTRY_NANOS: AtomicU64 = AtomicU64::new(0);
static RT_LAST_HEARTBEAT_NANOS: AtomicU64 = AtomicU64::new(0);
static RT_LAST_WATCHDOG_NANOS: AtomicU64 = AtomicU64::new(0);
static RT_EXECUTOR_ITERATIONS: AtomicU64 = AtomicU64::new(0);
static RT_TASK_STATS: [RtTaskStats; RT_TASK_COUNT] =
    [RtTaskStats::new(), RtTaskStats::new(), RtTaskStats::new()];

static RT_TASKS: [RtTask; RT_TASK_COUNT] = [
    RtTask::new("heartbeat", HEARTBEAT_INTERVAL_NANOS, heartbeat_task),
    RtTask::new("watchdog", WATCHDOG_INTERVAL_NANOS, watchdog_task),
    RtTask::new("hello", HELLO_INTERVAL_NANOS, hello_task),
];

static RT_RUNTIME: RtRuntime = RtRuntime::new();
static RT_OUTPUT: RtOutputBuffer = RtOutputBuffer::new();

/// Snapshot of the realtime CPU runtime state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RtStatus {
    /// Reserved realtime CPU ID, or `None` before the RT entry runs.
    pub cpu_id: Option<usize>,
    /// Current runtime state.
    pub state: RtState,
    /// Number of heartbeat periods observed by the RT loop.
    pub heartbeats: u64,
    /// Number of executor loop iterations.
    pub executor_iterations: u64,
    /// Number of static RT task contexts.
    pub task_count: usize,
    /// Monotonic timestamp when the RT entry started.
    pub entry_nanos: u64,
    /// Monotonic timestamp of the latest heartbeat.
    pub last_heartbeat_nanos: u64,
    /// Monotonic timestamp of the latest watchdog run.
    pub last_watchdog_nanos: u64,
    /// Static realtime task status table.
    pub tasks: [RtTaskStatus; RT_TASK_COUNT],
}

/// Snapshot of one static realtime task.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RtTaskStatus {
    /// Static task name.
    pub name: &'static str,
    /// Task period in nanoseconds.
    pub period_nanos: u64,
    /// Number of times the task callback ran.
    pub runs: u64,
    /// Current task scheduler state.
    pub state: RtTaskState,
    /// Deadline used while the task is delayed.
    pub deadline_nanos: u64,
    /// Latest callback start timestamp.
    pub last_start_nanos: u64,
    /// Latest callback finish timestamp.
    pub last_finish_nanos: u64,
}

/// Realtime task scheduler state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(usize)]
pub enum RtTaskState {
    /// Task can be selected by the RT executor.
    Ready = 0,
    /// Task is currently running on the RT CPU.
    Running = 1,
    /// Task is blocked until its deadline expires.
    Delayed = 2,
    /// Task is blocked on an RT synchronization primitive.
    Blocked = 3,
    /// Task finished and will not be scheduled again.
    Exited = 4,
}

/// A cooperative sleepable mutex for the isolated RT runtime.
pub struct RtMutex {
    owner: AtomicUsize,
    waiters: AtomicUsize,
}

impl RtMutex {
    /// Creates an unlocked RT mutex.
    pub const fn new() -> Self {
        Self {
            owner: AtomicUsize::new(usize::MAX),
            waiters: AtomicUsize::new(0),
        }
    }

    /// Locks the mutex, blocking the current RT task cooperatively if needed.
    pub fn lock(&self) -> RtMutexGuard<'_> {
        let task_id = RT_RUNTIME.current_running_task();
        loop {
            match self.owner.compare_exchange(
                usize::MAX,
                task_id,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return RtMutexGuard { mutex: self },
                Err(owner) if owner == task_id => {
                    panic!("RT mutex does not support recursive locking")
                }
                Err(_) => self.block_current_task(task_id),
            }
        }
    }

    fn block_current_task(&self, task_id: usize) {
        self.waiters.fetch_or(task_bit(task_id), Ordering::AcqRel);
        RT_TASK_STATS[task_id]
            .state
            .store(RtTaskState::Blocked as usize, Ordering::Release);
        rt_yield_now_with_state(task_id);
    }

    fn unlock(&self) {
        let task_id = RT_RUNTIME.current_running_task();
        self.owner
            .compare_exchange(task_id, usize::MAX, Ordering::AcqRel, Ordering::Acquire)
            .expect("RT mutex unlock must be called by the owner task");
        self.wake_one_waiter();
    }

    fn wake_one_waiter(&self) {
        loop {
            let waiters = self.waiters.load(Ordering::Acquire);
            if waiters == 0 {
                return;
            }
            let task_id = waiters.trailing_zeros() as usize;
            let task_mask = task_bit(task_id);
            if self
                .waiters
                .compare_exchange(
                    waiters,
                    waiters & !task_mask,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
            {
                RT_TASK_STATS[task_id]
                    .state
                    .store(RtTaskState::Ready as usize, Ordering::Release);
                return;
            }
        }
    }
}

/// Guard returned by [`RtMutex::lock`].
pub struct RtMutexGuard<'mutex> {
    mutex: &'mutex RtMutex,
}

impl Drop for RtMutexGuard<'_> {
    fn drop(&mut self) {
        self.mutex.unlock();
    }
}

static RT_SAMPLE_MUTEX: RtMutex = RtMutex::new();

struct RtOutputBuffer {
    write: AtomicUsize,
    read: AtomicUsize,
    dropped: AtomicU64,
    bytes: [AtomicU8; RT_OUTPUT_CAPACITY],
}

impl RtOutputBuffer {
    const fn new() -> Self {
        Self {
            write: AtomicUsize::new(0),
            read: AtomicUsize::new(0),
            dropped: AtomicU64::new(0),
            bytes: [const { AtomicU8::new(0) }; RT_OUTPUT_CAPACITY],
        }
    }

    fn push_bytes(&self, bytes: &[u8]) {
        for &byte in bytes {
            self.push_byte(byte);
        }
    }

    fn push_byte(&self, byte: u8) {
        let write = self.write.load(Ordering::Acquire);
        let read = self.read.load(Ordering::Acquire);
        if write.wrapping_sub(read) >= RT_OUTPUT_CAPACITY {
            self.dropped.fetch_add(1, Ordering::Relaxed);
            return;
        }
        self.bytes[write % RT_OUTPUT_CAPACITY].store(byte, Ordering::Release);
        self.write.store(write.wrapping_add(1), Ordering::Release);
    }

    fn pop_byte(&self) -> Option<u8> {
        let read = self.read.load(Ordering::Acquire);
        let write = self.write.load(Ordering::Acquire);
        if read == write {
            return None;
        }
        let byte = self.bytes[read % RT_OUTPUT_CAPACITY].load(Ordering::Acquire);
        self.read.store(read.wrapping_add(1), Ordering::Release);
        Some(byte)
    }
}

/// Realtime CPU entry state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(usize)]
pub enum RtState {
    /// The realtime CPU has not entered Axvisor yet.
    Offline = 0,
    /// The realtime CPU is executing the isolated cooperative executor.
    Running = 1,
}

#[derive(Clone, Copy)]
struct RtTask {
    name: &'static str,
    period_nanos: u64,
    run: fn() -> !,
}

impl RtTask {
    const fn new(name: &'static str, period_nanos: u64, run: fn() -> !) -> Self {
        Self {
            name,
            period_nanos,
            run,
        }
    }
}

struct RtTaskStats {
    runs: AtomicU64,
    state: AtomicUsize,
    deadline_nanos: AtomicU64,
    last_start_nanos: AtomicU64,
    last_finish_nanos: AtomicU64,
}

#[repr(align(16))]
struct RtTaskStack {
    bytes: UnsafeCell<[u8; RT_STACK_SIZE]>,
}

impl RtTaskStack {
    const fn new() -> Self {
        Self {
            bytes: UnsafeCell::new([0; RT_STACK_SIZE]),
        }
    }

    fn top(&self) -> usize {
        let base = self.bytes.get().cast::<u8>() as usize;
        align_down(base + RT_STACK_SIZE, RT_STACK_ALIGN)
    }
}

struct RtContext {
    context: UnsafeCell<MaybeUninit<TaskContext>>,
    header: CurrentThreadHeader,
    stack: RtTaskStack,
}

impl RtContext {
    const fn new(context_id: usize) -> Self {
        Self {
            context: UnsafeCell::new(MaybeUninit::uninit()),
            header: CurrentThreadHeader::new(
                CurrentContext::from_raw(context_id).expect("RT context IDs must be non-zero"),
            ),
            stack: RtTaskStack::new(),
        }
    }

    fn current_header(&self) -> Pin<&CurrentThreadHeader> {
        // SAFETY: RT contexts are stored in a static runtime and never moved.
        unsafe { Pin::new_unchecked(&self.header) }
    }

    fn context(&self) -> &TaskContext {
        // SAFETY: immutable context access is used only for the incoming task
        // while RT scheduling is serialized on the single reserved CPU.
        unsafe { (*self.context.get()).assume_init_ref() }
    }

    fn init_context(&self) {
        // SAFETY: each RT context is initialized exactly once before the RT
        // executor can switch to any task.
        unsafe { (*self.context.get()).write(TaskContext::new()) };
    }

    unsafe fn context_mut_ptr(&self) -> *mut TaskContext {
        // SAFETY: the caller owns the serialized RT scheduler transition that
        // permits mutable access to this initialized context.
        unsafe { (*self.context.get()).assume_init_mut() }
    }
}

struct RtRuntime {
    executor: RtContext,
    tasks: [RtContext; RT_TASK_COUNT],
    current_task: AtomicUsize,
    previous_binding: UnsafeCell<MaybeUninit<PreviousThreadBinding>>,
    has_previous_binding: AtomicUsize,
}

unsafe impl Sync for RtRuntime {}

impl RtRuntime {
    const fn new() -> Self {
        Self {
            executor: RtContext::new(EXECUTOR_CONTEXT_ID),
            tasks: [RtContext::new(2), RtContext::new(3), RtContext::new(4)],
            current_task: AtomicUsize::new(usize::MAX),
            previous_binding: UnsafeCell::new(MaybeUninit::uninit()),
            has_previous_binding: AtomicUsize::new(0),
        }
    }

    fn init_task_contexts(&self) {
        self.executor.init_context();
        // SAFETY: initialization runs once before any RT task can execute.
        unsafe { &mut *self.executor.context_mut_ptr() }
            .set_current_header(NonNull::from(&self.executor.header));
        // SAFETY: the RT entry runs after ax-runtime installed this CPU's
        // CPU-local area and before the CPU enters any ordinary scheduler path.
        unsafe {
            ax_std::os::arceos::modules::ax_hal::percpu::with_cpu_pin(|pin| {
                ax_std::os::arceos::modules::ax_hal::percpu::install_bootstrap_thread(
                    pin,
                    self.executor.current_header(),
                )
                .expect("RT executor bootstrap thread install failed")
            })
        }
        .expect("RT bootstrap requires an installed CPU-local area");

        for task_id in 0..RT_TASK_COUNT {
            let context = &self.tasks[task_id];
            context.init_context();
            let context_pointer = NonNull::from(&context.header);
            // SAFETY: initialization runs once before any RT task can execute.
            let task_context = unsafe { &mut *context.context_mut_ptr() };
            task_context.init(
                rt_task_entry as *const () as usize,
                ax_memory_addr::VirtAddr::from(context.stack.top()),
                KernelTlsBase::new(0),
            );
            task_context.set_current_header(context_pointer);
        }
    }

    fn switch_to_task(&self, task_id: usize) {
        self.current_task.store(task_id, Ordering::Release);
        self.switch_between(&self.executor, &self.tasks[task_id]);
    }

    fn switch_to_executor(&self, task_id: usize) {
        self.current_task.store(usize::MAX, Ordering::Release);
        self.switch_between(&self.tasks[task_id], &self.executor);
    }

    fn current_running_task(&self) -> usize {
        let task_id = self.current_task.load(Ordering::Acquire);
        assert!(
            task_id < RT_TASK_COUNT,
            "rt_yield_now must run in an RT task"
        );
        task_id
    }

    fn finish_previous_binding(&self, previous: &RtContext) {
        if self.has_previous_binding.swap(0, Ordering::AcqRel) == 0 {
            return;
        }

        // SAFETY: only the incoming RT context consumes the one stored previous
        // binding after a completed switch. The previous context is static.
        let binding = unsafe { (*self.previous_binding.get()).assume_init_read() };
        unsafe {
            binding
                .finish(previous.current_header())
                .expect("RT switch tail must match previous context")
        };
    }

    fn switch_between(&self, previous: &RtContext, next: &RtContext) {
        // SAFETY: RT scheduling is serialized on the reserved CPU; no RT task
        // migrates, and this path never enters the ordinary host scheduler.
        unsafe {
            ax_std::os::arceos::modules::ax_hal::percpu::with_cpu_pin(|pin| {
                let (prepared, previous_binding) =
                    ax_std::os::arceos::modules::ax_hal::percpu::prepare_thread_switch(
                        pin,
                        previous.current_header(),
                        next.current_header(),
                    )
                    .expect("RT context switch preparation failed");
                (&mut *previous.context_mut_ptr()).prepare_switch_to(next.context());

                (*self.previous_binding.get()).write(previous_binding);
                self.has_previous_binding.store(1, Ordering::Release);

                (&mut *previous.context_mut_ptr()).switch_to_prepared(next.context(), prepared);
            })
        }
        .expect("RT context switches require an installed CPU-local area");
    }
}

impl RtTaskStats {
    const fn new() -> Self {
        Self {
            runs: AtomicU64::new(0),
            state: AtomicUsize::new(RtTaskState::Ready as usize),
            deadline_nanos: AtomicU64::new(0),
            last_start_nanos: AtomicU64::new(0),
            last_finish_nanos: AtomicU64::new(0),
        }
    }

    fn snapshot(&self, task: &RtTask) -> RtTaskStatus {
        RtTaskStatus {
            name: task.name,
            period_nanos: task.period_nanos,
            runs: self.runs.load(Ordering::Relaxed),
            state: rt_task_state_from_usize(self.state.load(Ordering::Acquire)),
            deadline_nanos: self.deadline_nanos.load(Ordering::Acquire),
            last_start_nanos: self.last_start_nanos.load(Ordering::Acquire),
            last_finish_nanos: self.last_finish_nanos.load(Ordering::Acquire),
        }
    }

    fn is_ready(&self) -> bool {
        self.state.load(Ordering::Acquire) == RtTaskState::Ready as usize
    }
}

/// Axvisor realtime secondary CPU entry.
///
/// This symbol is called by `ax-runtime` after the reserved CPU has completed
/// minimal secondary CPU-local initialization and before it can enter the normal
/// host scheduler path.
#[unsafe(no_mangle)]
pub extern "Rust" fn ax_realtime_secondary_main(cpu_id: usize) -> ! {
    let entry_nanos = monotonic_time_nanos();
    RT_CPU_ID.store(cpu_id, Ordering::Release);
    RT_ENTRY_NANOS.store(entry_nanos, Ordering::Release);
    RT_LAST_HEARTBEAT_NANOS.store(entry_nanos, Ordering::Release);
    RT_LAST_WATCHDOG_NANOS.store(entry_nanos, Ordering::Release);
    RT_STATE.store(RtState::Running as usize, Ordering::Release);

    info!("Realtime CPU {cpu_id} entered Axvisor RT entry; running isolated executor.");
    let mut executor = RtExecutor::new(entry_nanos);
    executor.run()
}

struct RtExecutor;

impl RtExecutor {
    fn new(_now: u64) -> Self {
        Self
    }

    fn run(&mut self) -> ! {
        RT_RUNTIME.init_task_contexts();
        let mut next_task = 0usize;
        loop {
            RT_EXECUTOR_ITERATIONS.fetch_add(1, Ordering::Relaxed);
            let now = monotonic_time_nanos();
            wake_expired_tasks(now);
            if RT_TASK_STATS[next_task].is_ready() {
                self.run_task(next_task, now);
            }
            next_task = (next_task + 1) % RT_TASKS.len();
            rt_yield();
        }
    }

    fn run_task(&self, task_id: usize, now: u64) {
        let stats = &RT_TASK_STATS[task_id];
        stats
            .state
            .store(RtTaskState::Running as usize, Ordering::Release);
        stats.last_start_nanos.store(now, Ordering::Release);
        RT_RUNTIME.switch_to_task(task_id);
        RT_RUNTIME.finish_previous_binding(&RT_RUNTIME.tasks[task_id]);
    }
}

fn rt_task_entry() -> ! {
    let task_id = RT_RUNTIME.current_running_task();
    RT_RUNTIME.finish_previous_binding(&RT_RUNTIME.executor);
    (RT_TASKS[task_id].run)()
}

fn heartbeat_task() -> ! {
    let mut next_deadline = monotonic_time_nanos();
    loop {
        let now = monotonic_time_nanos();
        {
            let _guard = RT_SAMPLE_MUTEX.lock();
            RT_HEARTBEATS.fetch_add(1, Ordering::Relaxed);
            RT_LAST_HEARTBEAT_NANOS.store(now, Ordering::Release);
        }
        next_deadline = next_deadline.saturating_add(HEARTBEAT_INTERVAL_NANOS);
        if next_deadline <= monotonic_time_nanos() {
            rt_yield_now();
        } else {
            rt_delay_until(next_deadline);
        }
    }
}

fn watchdog_task() -> ! {
    loop {
        let now = monotonic_time_nanos();
        {
            let _guard = RT_SAMPLE_MUTEX.lock();
            RT_WATCHDOG_RUNS.fetch_add(1, Ordering::Relaxed);
            RT_LAST_WATCHDOG_NANOS.store(now, Ordering::Release);
        }
        rt_sleep(WATCHDOG_INTERVAL_NANOS);
    }
}

fn hello_task() -> ! {
    for index in 1..=HELLO_RUNS {
        rt_output_write(b"hello from RT task ");
        rt_output_write_decimal(index);
        rt_output_write(b"/5\n");
        rt_sleep(HELLO_INTERVAL_NANOS);
    }
    rt_exit_current_task();
}

fn rt_yield() {
    core::hint::spin_loop();
}

/// Yields the current RT task back to the isolated RT executor.
pub fn rt_yield_now() {
    let task_id = RT_RUNTIME.current_running_task();
    RT_TASK_STATS[task_id].runs.fetch_add(1, Ordering::Relaxed);
    RT_TASK_STATS[task_id]
        .last_finish_nanos
        .store(monotonic_time_nanos(), Ordering::Release);
    RT_TASK_STATS[task_id]
        .state
        .store(RtTaskState::Ready as usize, Ordering::Release);
    RT_RUNTIME.switch_to_executor(task_id);
    RT_RUNTIME.finish_previous_binding(&RT_RUNTIME.executor);
}

/// Blocks the current RT task until `deadline_nanos`.
pub fn rt_delay_until(deadline_nanos: u64) {
    let task_id = RT_RUNTIME.current_running_task();
    RT_TASK_STATS[task_id]
        .deadline_nanos
        .store(deadline_nanos, Ordering::Release);
    RT_TASK_STATS[task_id]
        .state
        .store(RtTaskState::Delayed as usize, Ordering::Release);
    rt_yield_now_with_state(task_id);
}

/// Blocks the current RT task for `duration_nanos`.
pub fn rt_sleep(duration_nanos: u64) {
    rt_delay_until(monotonic_time_nanos().saturating_add(duration_nanos));
}

/// Copies pending RT console output into `out` and returns the copied length.
pub fn rt_read_output(out: &mut [u8]) -> usize {
    let mut copied = 0;
    while copied < out.len() {
        let Some(byte) = RT_OUTPUT.pop_byte() else {
            break;
        };
        out[copied] = byte;
        copied += 1;
    }
    copied
}

fn rt_exit_current_task() -> ! {
    let task_id = RT_RUNTIME.current_running_task();
    RT_TASK_STATS[task_id]
        .state
        .store(RtTaskState::Exited as usize, Ordering::Release);
    rt_yield_now_with_state(task_id);
    loop {
        core::hint::spin_loop();
    }
}

fn rt_output_write(bytes: &[u8]) {
    RT_OUTPUT.push_bytes(bytes);
}

fn rt_output_write_decimal(mut value: u64) {
    let mut buffer = [0u8; 20];
    let mut index = buffer.len();
    if value == 0 {
        rt_output_write(b"0");
        return;
    }
    while value != 0 {
        index -= 1;
        buffer[index] = b'0' + (value % 10) as u8;
        value /= 10;
    }
    rt_output_write(&buffer[index..]);
}

fn rt_yield_now_with_state(task_id: usize) {
    RT_TASK_STATS[task_id].runs.fetch_add(1, Ordering::Relaxed);
    RT_TASK_STATS[task_id]
        .last_finish_nanos
        .store(monotonic_time_nanos(), Ordering::Release);
    RT_RUNTIME.switch_to_executor(task_id);
    RT_RUNTIME.finish_previous_binding(&RT_RUNTIME.executor);
}

fn wake_expired_tasks(now: u64) {
    for stats in &RT_TASK_STATS {
        if stats.state.load(Ordering::Acquire) == RtTaskState::Delayed as usize
            && now >= stats.deadline_nanos.load(Ordering::Acquire)
        {
            stats
                .state
                .store(RtTaskState::Ready as usize, Ordering::Release);
        }
    }
}

const fn align_down(value: usize, align: usize) -> usize {
    value & !(align - 1)
}

/// Returns the latest realtime CPU status snapshot.
pub fn status() -> RtStatus {
    let cpu_id = match RT_CPU_ID.load(Ordering::Acquire) {
        usize::MAX => None,
        cpu_id => Some(cpu_id),
    };

    RtStatus {
        cpu_id,
        state: rt_state_from_usize(RT_STATE.load(Ordering::Acquire)),
        heartbeats: RT_HEARTBEATS.load(Ordering::Relaxed),
        executor_iterations: RT_EXECUTOR_ITERATIONS.load(Ordering::Relaxed),
        task_count: RT_TASK_COUNT,
        entry_nanos: RT_ENTRY_NANOS.load(Ordering::Acquire),
        last_heartbeat_nanos: RT_LAST_HEARTBEAT_NANOS.load(Ordering::Acquire),
        last_watchdog_nanos: RT_LAST_WATCHDOG_NANOS.load(Ordering::Acquire),
        tasks: [
            RT_TASK_STATS[HEARTBEAT_TASK].snapshot(&RT_TASKS[HEARTBEAT_TASK]),
            RT_TASK_STATS[WATCHDOG_TASK].snapshot(&RT_TASKS[WATCHDOG_TASK]),
            RT_TASK_STATS[HELLO_TASK].snapshot(&RT_TASKS[HELLO_TASK]),
        ],
    }
}

fn rt_state_from_usize(value: usize) -> RtState {
    match value {
        value if value == RtState::Running as usize => RtState::Running,
        _ => RtState::Offline,
    }
}

fn rt_task_state_from_usize(value: usize) -> RtTaskState {
    match value {
        value if value == RtTaskState::Running as usize => RtTaskState::Running,
        value if value == RtTaskState::Delayed as usize => RtTaskState::Delayed,
        value if value == RtTaskState::Blocked as usize => RtTaskState::Blocked,
        value if value == RtTaskState::Exited as usize => RtTaskState::Exited,
        _ => RtTaskState::Ready,
    }
}

const fn task_bit(task_id: usize) -> usize {
    assert!(task_id < usize::BITS as usize);
    1usize << task_id
}

fn monotonic_time_nanos() -> u64 {
    ax_std::os::arceos::modules::ax_hal::time::monotonic_time_nanos()
}

/// Runtime owner of a physical CPU.
#[cfg(feature = "realtime")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CpuOwner {
    /// CPU is owned by the ordinary Axvisor host runtime.
    Host,
    /// CPU is reserved for the realtime runtime.
    Realtime,
    /// CPU is deliberately parked and not used by either runtime.
    Offline,
}

/// Returns the owner for `cpu_id`.
#[cfg(feature = "realtime")]
pub fn cpu_owner(cpu_id: usize) -> CpuOwner {
    if cpu_id >= runtime_cpu_count() {
        return CpuOwner::Offline;
    }
    if configured_realtime_cpu() == Some(cpu_id) {
        return CpuOwner::Realtime;
    }

    CpuOwner::Host
}

/// Logs the CPU ownership partition selected for this Axvisor build.
#[cfg(feature = "realtime")]
pub fn log_cpu_partition() {
    info!(
        "Axvisor realtime CPU partition: host_cpus={}, runtime_cpus={}",
        host_cpu_count(),
        runtime_cpu_count()
    );
    for cpu_id in 0..runtime_cpu_count() {
        debug!("  pCPU{cpu_id}: {:?}", cpu_owner(cpu_id));
    }
}

/// Returns whether `cpu_id` belongs to the ordinary Axvisor host runtime.
#[cfg(feature = "realtime")]
pub fn is_host_cpu(cpu_id: usize) -> bool {
    cpu_owner(cpu_id) == CpuOwner::Host
}

/// Returns the number of CPUs visible to the ordinary Axvisor host runtime.
#[cfg(feature = "realtime")]
pub fn host_cpu_count() -> usize {
    (0..runtime_cpu_count())
        .filter(|&cpu_id| is_host_cpu(cpu_id))
        .count()
}

#[cfg(feature = "realtime")]
fn runtime_cpu_count() -> usize {
    ax_std::thread::available_parallelism()
        .map(|count| count.get())
        .unwrap_or(1)
}

#[cfg(feature = "realtime")]
fn configured_realtime_cpu() -> Option<usize> {
    option_env!("AX_RT_CPU").and_then(parse_cpu_id)
}

#[cfg(feature = "realtime")]
fn parse_cpu_id(value: &str) -> Option<usize> {
    let value = value.trim();
    if let Some(hex) = value.strip_prefix("0x") {
        usize::from_str_radix(hex, 16).ok()
    } else {
        value.parse().ok()
    }
}
