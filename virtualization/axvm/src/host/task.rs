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
    spawn_and_wake_selected_cpu(
        || arceos::spawn_task(task),
        |task| task.cpu_id() as usize,
        arceos::send_ipi,
    )
}

fn spawn_and_wake_selected_cpu<T>(
    spawn: impl FnOnce() -> T,
    selected_cpu: impl FnOnce(&T) -> usize,
    wake: impl FnOnce(usize),
) -> T {
    let task = spawn();
    let cpu_id = selected_cpu(&task);
    wake(cpu_id);
    task
}

pub(crate) fn yield_now() {
    arceos::yield_now();
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

#[cfg(test)]
mod tests {
    use core::cell::RefCell;

    use super::spawn_and_wake_selected_cpu;

    #[test]
    fn spawned_task_is_enqueued_before_its_selected_cpu_is_woken() {
        let events = RefCell::new(alloc::vec::Vec::new());

        let task = spawn_and_wake_selected_cpu(
            || {
                events.borrow_mut().push("spawn");
                2
            },
            |cpu_id| {
                events.borrow_mut().push("select");
                *cpu_id
            },
            |cpu_id| {
                events
                    .borrow_mut()
                    .push(if cpu_id == 2 { "wake:2" } else { "wake" })
            },
        );

        assert_eq!(task, 2);
        assert_eq!(&*events.borrow(), &["spawn", "select", "wake:2"]);
    }
}
