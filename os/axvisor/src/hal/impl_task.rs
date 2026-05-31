use axvisor_api::task::{TaskHandle, TaskIf};

use crate::hal::task;

struct TaskImpl;

#[axvisor_api::api_impl]
impl TaskIf for TaskImpl {
    fn create_wait_queue() -> usize {
        task::create_wait_queue()
    }

    fn destroy_wait_queue(queue: usize) {
        task::destroy_wait_queue(queue)
    }

    fn wait_queue_wait(queue: usize) {
        task::wait_queue_wait(queue)
    }

    fn wait_queue_wait_until(
        queue: usize,
        condition: alloc::boxed::Box<dyn Fn() -> bool + Send + 'static>,
    ) {
        task::wait_queue_wait_until(queue, condition)
    }

    fn wait_queue_wake(queue: usize, count: u32) {
        task::wait_queue_wake(queue, count)
    }

    fn spawn_vcpu_task_raw(
        vm_id: usize,
        vcpu_id: usize,
        phys_cpu_set: Option<usize>,
        stack_size: usize,
        entry: alloc::boxed::Box<dyn FnOnce() + Send + 'static>,
    ) -> TaskHandle {
        task::spawn_vcpu_task_raw(vm_id, vcpu_id, phys_cpu_set, stack_size, entry)
    }

    fn task_id_name(task: TaskHandle) -> alloc::string::String {
        task::task_id_name(task)
    }

    fn task_cpu_id(task: TaskHandle) -> usize {
        task::task_cpu_id(task)
    }

    fn task_join(task: TaskHandle) -> i32 {
        task::task_join(task)
    }
}
