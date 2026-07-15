//! Host task type facade for AxVM's ArceOS-backed runtime.

use super::arceos;

pub(crate) type AxTaskRef = arceos::ArceOsAxTaskRef;
pub(crate) type CurrentTask = arceos::ArceOsCurrentTask;
pub(crate) type TaskError = arceos::ArceOsTaskError;
pub(crate) type TaskInner = arceos::ArceOsTaskInner;
pub(crate) type WaitQueue = arceos::ArceOsWaitQueue;
pub(crate) type WaitQueueHandle = arceos::ArceOsWaitQueueHandle;

pub(crate) fn current_task() -> CurrentTask {
    assert!(
        !in_hard_irq(),
        "AxVM cannot acquire an owning task handle from hard IRQ context"
    );
    match try_current_task() {
        Ok(Some(task)) => task,
        Ok(None) => panic!("AxVM current task is unavailable"),
        Err(error) => panic!("AxVM current task identity is invalid: {error}"),
    }
}

pub(crate) fn try_current_task() -> Result<Option<CurrentTask>, TaskError> {
    arceos::try_current_task()
}

pub(crate) fn in_hard_irq() -> bool {
    arceos::in_hard_irq()
}

pub(crate) fn spawn_task(task: TaskInner) -> AxTaskRef {
    arceos::spawn_task(task)
}

pub(crate) fn yield_now() {
    arceos::yield_now();
}

pub(crate) fn cpu_mask_from_raw_bits(bits: usize) -> arceos::ArceOsCpuMask {
    arceos::cpu_mask_from_raw_bits(bits)
}

pub(crate) fn cpu_mask_one_shot(cpu_id: usize) -> arceos::ArceOsCpuMask {
    arceos::cpu_mask_one_shot(cpu_id)
}

pub(crate) fn wait_queue_wait_until(queue: &WaitQueueHandle, condition: impl Fn() -> bool) {
    arceos::wait_queue_wait_until(queue, condition);
}

pub(crate) fn wait_queue_wait_until_deadline(
    queue: &WaitQueueHandle,
    deadline: core::time::Duration,
    condition: impl Fn() -> bool,
) -> bool {
    arceos::wait_queue_wait_until_deadline(queue, deadline, condition)
}

pub(crate) fn wait_queue_wake(queue: &WaitQueueHandle, count: u32) {
    arceos::wait_queue_wake(queue, count);
}

pub(crate) fn send_ipi(cpu_id: usize) -> crate::AxVmResult {
    arceos::send_ipi(cpu_id)
}
