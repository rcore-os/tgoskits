use alloc::{
    format,
    string::String,
    sync::{Arc, Weak},
};

use std::os::arceos::{
    api::task::{self, AxCpuMask, AxWaitQueueHandle},
    modules::ax_task::{self as host_task, AxTaskExt, AxTaskRef, TaskExt, TaskInner, WaitQueue},
};

use crate::vmm::{VCpuRef, VM, VMRef};

/// Task extended data for the hypervisor.
pub(crate) struct VCpuTask {
    /// The VM (Weak reference to avoid keeping VM alive).
    pub vm: Weak<VM>,
    /// The virtual CPU.
    pub vcpu: VCpuRef,
}

impl VCpuTask {
    /// Create a new [`VCpuTask`].
    pub fn new(vm: &VMRef, vcpu: VCpuRef) -> Self {
        Self {
            vm: Arc::downgrade(vm),
            vcpu,
        }
    }

    /// Get a strong reference to the VM.
    pub fn vm(&self) -> VMRef {
        self.vm.upgrade().expect("VM has been dropped")
    }
}

#[extern_trait::extern_trait]
impl TaskExt for VCpuTask {}

pub(crate) trait AsVCpuTask {
    fn try_as_vcpu_task(&self) -> Option<&VCpuTask>;

    #[track_caller]
    fn as_vcpu_task(&self) -> &VCpuTask;
}

impl AsVCpuTask for TaskInner {
    fn try_as_vcpu_task(&self) -> Option<&VCpuTask> {
        self.task_ext().map(|ext| ext.downcast_ref::<VCpuTask>())
    }

    fn as_vcpu_task(&self) -> &VCpuTask {
        self.try_as_vcpu_task().expect("Not a VCpuTask")
    }
}

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
