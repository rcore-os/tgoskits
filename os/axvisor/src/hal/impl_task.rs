use alloc::{boxed::Box, collections::BTreeMap, sync::Arc};
use core::sync::atomic::{AtomicBool, Ordering};

use ax_kspin::SpinNoIrq;
use axvisor_api::task::{TaskHandle, TaskIf, TaskOptions};
use std::os::arceos::{
    api::task::AxCpuMask,
    modules::ax_task::{self as host_task, AxTaskRef, TaskInner},
};

pub(crate) struct HostTaskExt;

#[extern_trait::extern_trait]
impl host_task::TaskExt for HostTaskExt {}

static TASKS: SpinNoIrq<BTreeMap<usize, AxTaskRef>> = SpinNoIrq::new(BTreeMap::new());

fn get_task(task: TaskHandle) -> AxTaskRef {
    TASKS
        .lock()
        .get(&task.as_raw())
        .cloned()
        .expect("task handle not found")
}

struct TaskImpl;

#[axvisor_api::api_impl]
impl TaskIf for TaskImpl {
    fn spawn_task_raw(
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

    fn join_task(task: TaskHandle) {
        let task_ref = get_task(task);
        task_ref.join();
        TASKS.lock().remove(&task.as_raw());
    }

    fn current_task() -> Option<TaskHandle> {
        host_task::current_may_uninit()
            .map(|task| TaskHandle::from_raw(task.id().as_u64() as usize))
    }

    fn yield_now() {
        std::thread::yield_now();
    }
}
