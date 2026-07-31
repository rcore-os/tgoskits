//! Host task type facade for AxVM's ArceOS-backed runtime.

use super::arceos;

pub(crate) type TaskHandle = arceos::ArceOsTaskHandle;
pub(crate) type TaskExtensionBorrow<'task> =
    ax_std::os::arceos::modules::ax_runtime::task::ThreadOsExtensionBorrow<'task>;
pub(crate) type WaitQueue = arceos::ArceOsWaitQueue;
pub(crate) type WaitQueueHandle = arceos::ArceOsWaitQueueHandle;
pub(crate) use arceos::{
    ArceOsSchedulePolicy as SchedulePolicy, ArceOsSwitchReason as SwitchReason,
    ArceOsTaskCpuSet as TaskCpuSet, ArceOsTaskError as TaskError,
    ArceOsThreadExtension as ThreadExtension, ArceOsThreadExtensionOps as ThreadExtensionOps,
    ArceOsThreadId as ThreadId,
};

pub(crate) fn current_task() -> TaskHandle {
    arceos::current_task()
}

pub(crate) unsafe fn spawn_task_with_extension_and_affinity<F>(
    entry: F,
    name: alloc::string::String,
    stack_size: usize,
    extension: Option<ThreadExtension>,
    affinity: Option<TaskCpuSet>,
) -> Result<TaskHandle, TaskError>
where
    F: FnOnce() + Send + 'static,
{
    // SAFETY: the caller transfers the unique extension ownership through this
    // one-to-one host adapter.
    unsafe {
        arceos::spawn_task_with_extension_and_affinity(entry, name, stack_size, extension, affinity)
    }
}

pub(crate) fn join_task(task: TaskHandle) -> Result<i32, TaskError> {
    arceos::join_task(task)
}

pub(crate) fn task_extension(
    task: &TaskHandle,
) -> Result<Option<TaskExtensionBorrow<'_>>, TaskError> {
    arceos::task_extension(task)
}

pub(crate) fn yield_now() {
    arceos::yield_now();
}

pub(crate) fn task_cpu_set_from_raw_bits(bits: usize) -> TaskCpuSet {
    arceos::task_cpu_set_from_raw_bits(bits)
}

pub(crate) fn task_cpu_id(task: &TaskHandle) -> usize {
    arceos::task_cpu_id(task)
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
