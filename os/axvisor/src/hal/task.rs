use alloc::{boxed::Box, collections::BTreeMap, sync::Arc};
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use ax_kspin::SpinNoIrq;
use axvisor_api::task::{TaskHandle, TaskOptions};
use std::os::arceos::{
    api::task::{
        AxCpuMask, AxWaitQueueHandle, ax_wait_queue_wait, ax_wait_queue_wait_until,
        ax_wait_queue_wake,
    },
    modules::ax_task::{self as host_task, AxTaskRef, TaskInner},
};

pub(crate) struct HostTaskExt;

#[extern_trait::extern_trait]
impl host_task::TaskExt for HostTaskExt {}

static WAIT_QUEUE_IDS: AtomicUsize = AtomicUsize::new(1);
static WAIT_QUEUES: SpinNoIrq<BTreeMap<usize, Arc<AxWaitQueueHandle>>> =
    SpinNoIrq::new(BTreeMap::new());
static TASKS: SpinNoIrq<BTreeMap<usize, AxTaskRef>> = SpinNoIrq::new(BTreeMap::new());

fn get_wait_queue(queue: usize) -> Arc<AxWaitQueueHandle> {
    WAIT_QUEUES
        .lock()
        .get(&queue)
        .cloned()
        .expect("wait queue not found")
}

fn get_task(task: TaskHandle) -> AxTaskRef {
    TASKS
        .lock()
        .get(&task.as_raw())
        .cloned()
        .expect("task handle not found")
}

pub(crate) fn create_wait_queue() -> usize {
    let id = WAIT_QUEUE_IDS.fetch_add(1, Ordering::Relaxed);
    WAIT_QUEUES
        .lock()
        .insert(id, Arc::new(AxWaitQueueHandle::new()));
    id
}

pub(crate) fn destroy_wait_queue(queue: usize) {
    WAIT_QUEUES.lock().remove(&queue);
}

pub(crate) fn wait_queue_wait(queue: usize) {
    let queue = get_wait_queue(queue);
    ax_wait_queue_wait(queue.as_ref(), None);
}

pub(crate) fn wait_queue_wait_until(
    queue: usize,
    condition: Box<dyn Fn() -> bool + Send + 'static>,
) {
    let queue = get_wait_queue(queue);
    ax_wait_queue_wait_until(queue.as_ref(), condition, None);
}

pub(crate) fn wait_queue_wake(queue: usize, count: u32) {
    let queue = get_wait_queue(queue);
    ax_wait_queue_wake(queue.as_ref(), count);
}

pub(crate) fn spawn_task_raw(
    options: TaskOptions,
    entry: Box<dyn FnOnce() + Send + 'static>,
) -> TaskHandle {
    let registered = Arc::new(AtomicBool::new(false));
    let registered_for_task = registered.clone();
    let task = TaskInner::new(
        move || {
            while !registered_for_task.load(Ordering::Acquire) {
                std::thread::yield_now();
            }
            entry();
        },
        options.name,
        options.stack_size,
    );

    if let Some(cpu_set) = options.cpu_set {
        task.set_cpumask(AxCpuMask::from_raw_bits(cpu_set));
    }

    let task = host_task::spawn_task(task);
    let handle = TaskHandle::from_raw(task.id().as_u64() as usize);
    TASKS.lock().insert(handle.as_raw(), task);
    registered.store(true, Ordering::Release);
    handle
}

pub(crate) fn join_task(task: TaskHandle) {
    let task_ref = get_task(task);
    task_ref.join();
    TASKS.lock().remove(&task.as_raw());
}

pub(crate) fn current_task() -> Option<TaskHandle> {
    host_task::current_may_uninit().map(|task| TaskHandle::from_raw(task.id().as_u64() as usize))
}

pub(crate) fn yield_now() {
    std::thread::yield_now();
}
