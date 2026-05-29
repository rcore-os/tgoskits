use ax_std::os::arceos::{api, modules};

pub use modules::ax_task::{AxTaskExt, AxTaskRef, CurrentTask, TaskExt, TaskInner, WaitQueue};

pub type HostWaitQueueHandle = api::task::AxWaitQueueHandle;

pub fn current() -> CurrentTask {
    modules::ax_task::current()
}

pub fn spawn_task(task: TaskInner) -> AxTaskRef {
    modules::ax_task::spawn_task(task)
}

pub fn wait_queue_wait_until(
    queue: &HostWaitQueueHandle,
    condition: impl Fn() -> bool,
    timeout: Option<api::time::AxTimeValue>,
) {
    api::task::ax_wait_queue_wait_until(queue, condition, timeout);
}

pub fn wait_queue_wake(queue: &HostWaitQueueHandle, count: u32) {
    api::task::ax_wait_queue_wake(queue, count);
}
