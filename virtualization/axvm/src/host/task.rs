//! Host task type facade for AxVM's ArceOS-backed runtime.

use super::arceos;

pub(crate) type ThreadHandle = arceos::ArceOsThreadHandle;
pub(crate) type IrqNotification = arceos::ArceOsIrqNotification;
pub(crate) type MonotonicDeadline = arceos::ArceOsMonotonicDeadline;
pub(crate) type KernelTimerHandle = arceos::ArceOsKernelTimerHandle;
pub(crate) type KernelTimerAction = arceos::ArceOsKernelTimerAction;
pub(crate) type ThreadExtensionBorrow<'thread> =
    ax_std::os::arceos::task::ThreadOsExtensionBorrow<'thread>;
pub(crate) type WaitQueue = arceos::ArceOsWaitQueue;
pub(crate) type WaitQueueHandle = arceos::ArceOsWaitQueueHandle;
pub(crate) use arceos::{
    ArceOsCpuSet as CpuSet, ArceOsSchedulePolicy as SchedulePolicy,
    ArceOsSwitchReason as SwitchReason, ArceOsTaskError as TaskError,
    ArceOsThreadExtension as ThreadExtension, ArceOsThreadExtensionOps as ThreadExtensionOps,
    ArceOsThreadId as ThreadId,
};

pub(crate) fn current_thread() -> ThreadHandle {
    arceos::current_thread()
}

pub(crate) fn register_kernel_timer(
    deadline: MonotonicDeadline,
    callback: std::boxed::Box<dyn FnOnce(std::time::Duration) + Send + 'static>,
) -> Result<KernelTimerHandle, TaskError> {
    arceos::register_kernel_timer(deadline, callback)
}

pub(crate) fn register_restartable_kernel_timer(
    deadline: MonotonicDeadline,
    callback: std::boxed::Box<dyn FnMut(std::time::Duration) -> KernelTimerAction + Send + 'static>,
) -> Result<KernelTimerHandle, TaskError> {
    arceos::register_restartable_kernel_timer(deadline, callback)
}

#[cfg(any(
    target_arch = "aarch64",
    target_arch = "loongarch64",
    target_arch = "x86_64"
))]
pub(crate) fn cancel_kernel_timer(handle: KernelTimerHandle) -> Result<bool, TaskError> {
    arceos::cancel_kernel_timer(handle)
}

pub(crate) unsafe fn spawn_thread_with_extension_and_affinity<F>(
    entry: F,
    name: std::string::String,
    stack_size: usize,
    extension: Option<ThreadExtension>,
    affinity: Option<CpuSet>,
) -> Result<ThreadHandle, TaskError>
where
    F: FnOnce() + Send + 'static,
{
    // SAFETY: the caller transfers the unique extension ownership through this
    // one-to-one host adapter.
    unsafe {
        arceos::spawn_thread_with_extension_and_affinity(
            entry, name, stack_size, extension, affinity,
        )
    }
}

pub(crate) fn join_thread(thread: ThreadHandle) -> Result<i32, TaskError> {
    arceos::join_thread(thread)
}

pub(crate) fn thread_extension(
    thread: &ThreadHandle,
) -> Result<Option<ThreadExtensionBorrow<'_>>, TaskError> {
    arceos::thread_extension(thread)
}

pub(crate) fn yield_now() {
    arceos::yield_now();
}

pub(crate) fn cpu_set_from_raw_bits(bits: usize) -> CpuSet {
    arceos::cpu_set_from_raw_bits(bits)
}

pub(crate) fn thread_cpu_id(thread: &ThreadHandle) -> Option<usize> {
    arceos::thread_cpu_id(thread)
}

#[cfg(target_arch = "aarch64")]
pub(crate) fn run_on_cpu_sync(
    cpu_id: usize,
    operation: unsafe fn(*mut ()),
    argument: *mut (),
) -> Result<(), arceos::ArceOsIrqError> {
    arceos::run_on_cpu_sync(cpu_id, operation, argument)
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
