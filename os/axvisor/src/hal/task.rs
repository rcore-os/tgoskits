use alloc::{boxed::Box, collections::BTreeMap, format, string::String, sync::Arc};
use core::sync::atomic::{AtomicUsize, Ordering};

use ax_kspin::SpinNoIrq;
use axvisor_api::task::TaskHandle;
use std::os::arceos::{
    api::task::{
        AxCpuMask, AxWaitQueueHandle, ax_wait_queue_wait, ax_wait_queue_wait_until,
        ax_wait_queue_wake,
    },
    modules::ax_task::{self as host_task, AxTaskExt, AxTaskRef, TaskExt, TaskInner},
};

/// Task-local vCPU execution context attached to each spawned vCPU host task.
pub(crate) struct VCpuTaskContext {
    vm_id: usize,
    vcpu_id: usize,
}

impl VCpuTaskContext {
    pub const fn new(vm_id: usize, vcpu_id: usize) -> Self {
        Self { vm_id, vcpu_id }
    }
}

#[extern_trait::extern_trait]
impl TaskExt for VCpuTaskContext {}

trait AsVCpuTaskContext {
    fn as_vcpu_task_context(&self) -> &VCpuTaskContext;
}

impl AsVCpuTaskContext for TaskInner {
    fn as_vcpu_task_context(&self) -> &VCpuTaskContext {
        self.task_ext()
            .expect("Not a vCPU task")
            .downcast_ref::<VCpuTaskContext>()
    }
}

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

pub(crate) fn spawn_vcpu_task_raw(
    vm_id: usize,
    vcpu_id: usize,
    phys_cpu_set: Option<usize>,
    stack_size: usize,
    entry: Box<dyn FnOnce() + Send + 'static>,
) -> TaskHandle {
    let mut vcpu_task = TaskInner::new(
        move || entry(),
        format!("VM[{vm_id}]-VCpu[{vcpu_id}]"),
        stack_size,
    );

    if let Some(phys_cpu_set) = phys_cpu_set {
        vcpu_task.set_cpumask(AxCpuMask::from_raw_bits(phys_cpu_set));
    }

    *vcpu_task.task_ext_mut() = Some(AxTaskExt::from_impl(VCpuTaskContext::new(vm_id, vcpu_id)));

    let task = host_task::spawn_task(vcpu_task);
    let handle = TaskHandle::from_raw(task.id().as_u64() as usize);
    TASKS.lock().insert(handle.as_raw(), task);
    handle
}

pub(crate) fn task_id_name(task: TaskHandle) -> String {
    get_task(task).id_name()
}

pub(crate) fn task_cpu_id(task: TaskHandle) -> usize {
    get_task(task).cpu_id() as usize
}

pub(crate) fn task_join(task: TaskHandle) -> i32 {
    let task_ref = get_task(task);
    let exit_code = task_ref.join();
    TASKS.lock().remove(&task.as_raw());
    exit_code
}

pub(crate) fn current_vm_id() -> usize {
    host_task::current().as_vcpu_task_context().vm_id
}

pub(crate) fn current_vcpu_id() -> usize {
    host_task::current().as_vcpu_task_context().vcpu_id
}
