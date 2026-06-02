//! Host task type facade for AxVM's ArceOS-backed runtime.

use super::arceos;

pub(crate) type AxTaskExt = arceos::ArceOsAxTaskExt;
pub(crate) type AxTaskRef = arceos::ArceOsAxTaskRef;
pub(crate) type CurrentTask = arceos::ArceOsCurrentTask;
pub(crate) type TaskInner = arceos::ArceOsTaskInner;
pub(crate) type WaitQueue = arceos::ArceOsWaitQueue;
pub(crate) type WaitQueueHandle = arceos::ArceOsWaitQueueHandle;
pub(crate) use arceos::ArceOsTaskExt as TaskExt;

pub(crate) fn current_task() -> CurrentTask {
    arceos::current_task()
}

pub(crate) fn spawn_task(task: TaskInner) -> AxTaskRef {
    arceos::spawn_task(task)
}

pub(crate) fn cpu_mask_from_raw_bits(bits: usize) -> arceos::ArceOsCpuMask {
    arceos::cpu_mask_from_raw_bits(bits)
}

pub(crate) fn wait_queue_wait_until(queue: &WaitQueueHandle, condition: impl Fn() -> bool) {
    arceos::wait_queue_wait_until(queue, condition);
}

pub(crate) fn wait_queue_wake(queue: &WaitQueueHandle, count: u32) {
    arceos::wait_queue_wake(queue, count);
}

pub(crate) fn send_ipi(cpu_id: usize) {
    arceos::send_ipi(cpu_id);
}
