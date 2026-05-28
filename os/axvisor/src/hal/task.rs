use alloc::{format, string::String};

use std::os::arceos::{
    api::task::{self, AxCpuMask, AxWaitQueueHandle},
    modules::ax_task::{self as host_task, AxTaskExt, AxTaskRef, TaskInner, WaitQueue},
};

use crate::{
    task::{AsVCpuTask, VCpuTask},
    vmm::{VCpuRef, VMRef},
};

pub(crate) struct VmmWaitQueue(AxWaitQueueHandle);

impl VmmWaitQueue {
    pub const fn new() -> Self {
        Self(AxWaitQueueHandle::new())
    }

    pub fn wait_until<F>(&self, condition: F)
    where
        F: Fn() -> bool,
    {
        task::ax_wait_queue_wait_until(&self.0, condition, None);
    }

    pub fn wake(&self, count: u32) {
        task::ax_wait_queue_wake(&self.0, count);
    }
}

pub(crate) struct VmWaitQueue(WaitQueue);

impl VmWaitQueue {
    pub fn new() -> Self {
        Self(WaitQueue::new())
    }

    pub fn wait(&self) {
        self.0.wait();
    }

    pub fn wait_until<F>(&self, condition: F)
    where
        F: Fn() -> bool,
    {
        self.0.wait_until(condition);
    }

    pub fn notify_one(&self) {
        self.0.notify_one(false);
    }

    pub fn notify_all(&self) {
        self.0.notify_all(false);
    }
}

#[derive(Clone)]
pub(crate) struct TaskRef(AxTaskRef);

impl TaskRef {
    pub fn id_name(&self) -> String {
        format!("{}", self.0.id_name())
    }

    pub fn cpu_id(&self) -> usize {
        self.0.cpu_id() as usize
    }

    pub fn join(&self) -> i32 {
        self.0.join()
    }
}

pub(crate) fn spawn_vcpu_task(
    vm: &VMRef,
    vcpu: VCpuRef,
    stack_size: usize,
    entry: fn(),
) -> TaskRef {
    let mut vcpu_task = TaskInner::new(
        entry,
        format!("VM[{}]-VCpu[{}]", vm.id(), vcpu.id()),
        stack_size,
    );

    if let Some(phys_cpu_set) = vcpu.phys_cpu_set() {
        vcpu_task.set_cpumask(AxCpuMask::from_raw_bits(phys_cpu_set));
    }

    let inner = VCpuTask::new(vm, vcpu);
    *vcpu_task.task_ext_mut() = Some(AxTaskExt::from_impl(inner));

    TaskRef(host_task::spawn_task(vcpu_task))
}

pub(crate) fn current_vm_vcpu() -> (VMRef, VCpuRef) {
    let current = host_task::current();
    let current = current.as_vcpu_task();
    (current.vm(), current.vcpu.clone())
}

pub(crate) fn current_vm_id() -> usize {
    host_task::current().as_vcpu_task().vm().id()
}

pub(crate) fn current_vcpu_id() -> usize {
    host_task::current().as_vcpu_task().vcpu.id()
}
