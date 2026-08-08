//! RT executor lifecycle, sleep/yield APIs, and global task registry.

use core::sync::atomic::{AtomicPtr, AtomicU64, AtomicUsize, Ordering};

use crate::{
    MAX_RT_TASKS,
    context::RT_RUNTIME,
    state::{RtState, RtTaskState, rt_state_from_usize},
    task::{RtStatus, RtTask, RtTaskStats, RtTaskStatus},
};

static RT_CPU_ID: AtomicUsize = AtomicUsize::new(usize::MAX);
static RT_STATE: AtomicUsize = AtomicUsize::new(RtState::Offline as usize);
static RT_ENTRY_NANOS: AtomicU64 = AtomicU64::new(0);
static RT_EXECUTOR_ITERATIONS: AtomicU64 = AtomicU64::new(0);
static RT_TASKS: AtomicPtr<RtTask> = AtomicPtr::new(core::ptr::null_mut());
static RT_TASK_COUNT: AtomicUsize = AtomicUsize::new(0);
static RT_TIME_SOURCE: AtomicUsize = AtomicUsize::new(0);
pub(crate) static RT_TASK_STATS: [RtTaskStats; MAX_RT_TASKS] =
    [const { RtTaskStats::new() }; MAX_RT_TASKS];

/// Runs the isolated RT executor on the current CPU.
pub fn run_realtime_cpu(cpu_id: usize, tasks: &'static [RtTask], time_source: fn() -> u64) -> ! {
    assert!(!tasks.is_empty(), "RT executor requires at least one task");
    assert!(
        tasks.len() <= MAX_RT_TASKS,
        "RT executor supports at most {MAX_RT_TASKS} tasks"
    );

    let entry_nanos = time_source();
    RT_TASKS.store(tasks.as_ptr().cast_mut(), Ordering::Release);
    RT_TASK_COUNT.store(tasks.len(), Ordering::Release);
    RT_TIME_SOURCE.store(time_source as usize, Ordering::Release);
    RT_CPU_ID.store(cpu_id, Ordering::Release);
    RT_ENTRY_NANOS.store(entry_nanos, Ordering::Release);
    RT_STATE.store(RtState::Running as usize, Ordering::Release);

    RtExecutor.run()
}

/// Yields the current RT task back to the isolated RT executor.
pub fn rt_yield_now() {
    let task_id = current_running_task();
    RT_TASK_STATS[task_id]
        .state
        .store(RtTaskState::Ready as usize, Ordering::Release);
    yield_current_task_with_state(task_id);
}

/// Blocks the current RT task until `deadline_nanos`.
pub fn rt_delay_until(deadline_nanos: u64) {
    let task_id = current_running_task();
    RT_TASK_STATS[task_id]
        .deadline_nanos
        .store(deadline_nanos, Ordering::Release);
    RT_TASK_STATS[task_id]
        .state
        .store(RtTaskState::Delayed as usize, Ordering::Release);
    yield_current_task_with_state(task_id);
}

/// Blocks the current RT task for `duration_nanos`.
pub fn rt_sleep(duration_nanos: u64) {
    rt_delay_until(monotonic_time_nanos().saturating_add(duration_nanos));
}

/// Marks the current RT task as exited and never schedules it again.
pub fn rt_exit_current_task() -> ! {
    let task_id = current_running_task();
    RT_TASK_STATS[task_id]
        .state
        .store(RtTaskState::Exited as usize, Ordering::Release);
    yield_current_task_with_state(task_id);
    loop {
        core::hint::spin_loop();
    }
}

/// Returns the latest realtime CPU status snapshot.
pub fn status() -> RtStatus {
    let cpu_id = match RT_CPU_ID.load(Ordering::Acquire) {
        usize::MAX => None,
        cpu_id => Some(cpu_id),
    };
    let task_count = rt_task_count();
    let mut task_status = [RtTaskStatus::empty(); MAX_RT_TASKS];
    for task_id in 0..task_count {
        task_status[task_id] = RT_TASK_STATS[task_id].snapshot(rt_task(task_id));
    }

    RtStatus {
        cpu_id,
        state: rt_state_from_usize(RT_STATE.load(Ordering::Acquire)),
        executor_iterations: RT_EXECUTOR_ITERATIONS.load(Ordering::Relaxed),
        task_count,
        entry_nanos: RT_ENTRY_NANOS.load(Ordering::Acquire),
        tasks: task_status,
    }
}

pub(crate) fn current_running_task() -> usize {
    RT_RUNTIME.current_running_task(rt_task_count())
}

pub(crate) fn yield_current_task_with_state(task_id: usize) {
    RT_TASK_STATS[task_id].runs.fetch_add(1, Ordering::Relaxed);
    RT_TASK_STATS[task_id]
        .last_finish_nanos
        .store(monotonic_time_nanos(), Ordering::Release);
    RT_RUNTIME.switch_to_executor(task_id);
    RT_RUNTIME.finish_previous_binding(&RT_RUNTIME.executor);
}

pub(crate) fn rt_task_entry() -> ! {
    let task_id = current_running_task();
    RT_RUNTIME.finish_previous_binding(&RT_RUNTIME.executor);
    (rt_task(task_id).run)()
}

struct RtExecutor;

impl RtExecutor {
    fn run(&mut self) -> ! {
        RT_RUNTIME.init_task_contexts(rt_task_count());
        let mut next_task = 0usize;
        loop {
            RT_EXECUTOR_ITERATIONS.fetch_add(1, Ordering::Relaxed);
            let now = monotonic_time_nanos();
            wake_expired_tasks(now);
            if RT_TASK_STATS[next_task].is_ready() {
                self.run_task(next_task, now);
            }
            next_task = (next_task + 1) % rt_task_count();
            core::hint::spin_loop();
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

fn wake_expired_tasks(now: u64) {
    for stats in &RT_TASK_STATS[..rt_task_count()] {
        if stats.state.load(Ordering::Acquire) == RtTaskState::Delayed as usize
            && now >= stats.deadline_nanos.load(Ordering::Acquire)
        {
            stats
                .state
                .store(RtTaskState::Ready as usize, Ordering::Release);
        }
    }
}

fn rt_task(task_id: usize) -> &'static RtTask {
    assert!(task_id < rt_task_count(), "invalid RT task id {task_id}");
    let tasks = RT_TASKS.load(Ordering::Acquire);
    assert!(!tasks.is_null(), "RT tasks are not initialized");
    // SAFETY: `run_realtime_cpu` stores a valid static task slice before any RT
    // scheduling can occur, and `task_id` was range-checked above.
    unsafe { &*tasks.add(task_id) }
}

fn rt_task_count() -> usize {
    RT_TASK_COUNT.load(Ordering::Acquire)
}

fn monotonic_time_nanos() -> u64 {
    let raw = RT_TIME_SOURCE.load(Ordering::Acquire);
    assert!(raw != 0, "RT time source is not initialized");
    // SAFETY: `run_realtime_cpu` stores a valid `fn() -> u64` pointer before any
    // RT task can call time-dependent APIs.
    let source: fn() -> u64 = unsafe { core::mem::transmute(raw) };
    source()
}
