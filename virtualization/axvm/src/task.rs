//! Host task extension data used by AxVM vCPU tasks.

extern crate alloc;

use alloc::sync::{Arc, Weak};

use crate::{
    host::task::{TaskExt, TaskInner},
    vm::{AxVCpuRef, AxVMRef},
};

/// Task extended data for a vCPU host task.
pub struct VCpuTask {
    /// The VM. Stored weakly to avoid keeping a VM alive through its task.
    pub vm: Weak<crate::AxVM>,
    /// The virtual CPU.
    pub vcpu: AxVCpuRef,
}

impl VCpuTask {
    /// Create a new vCPU task extension.
    pub fn new(vm: &AxVMRef, vcpu: AxVCpuRef) -> Self {
        Self {
            vm: Arc::downgrade(vm),
            vcpu,
        }
    }

    /// Get a strong reference to the VM.
    ///
    /// # Panics
    ///
    /// Panics if the VM has already been dropped.
    pub fn vm(&self) -> AxVMRef {
        self.vm.upgrade().expect("VM has been dropped")
    }
}

#[extern_trait::extern_trait]
impl TaskExt for VCpuTask {}

/// Access a vCPU task extension from an ArceOS task.
pub trait AsVCpuTask {
    /// Return this task's vCPU extension if it has one.
    fn try_as_vcpu_task(&self) -> Option<&VCpuTask>;

    /// Return this task's vCPU extension.
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
