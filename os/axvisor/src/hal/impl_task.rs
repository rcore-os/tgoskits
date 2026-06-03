use axvisor_api::task::{TaskHandle, TaskIf, TaskOptions};

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

    fn spawn_task_raw(
        options: TaskOptions,
        entry: alloc::boxed::Box<dyn FnOnce() + Send + 'static>,
    ) -> TaskHandle {
        task::spawn_task_raw(options, entry)
    }

    fn join_task(task: TaskHandle) {
        task::join_task(task)
    }

    fn current_task() -> Option<TaskHandle> {
        task::current_task()
    }

    fn yield_now() {
        task::yield_now()
    }
}
