use alloc::sync::{Arc, Weak};
use std::os::arceos::modules::axtask::{TaskExt, TaskInner};

use crate::vmm::{VCpuRef, VM, VMRef};

/// Task extended data for the hypervisor.
pub struct VCpuTask {
    /// The VM (Weak reference to avoid keeping VM alive).
    pub vm: Weak<VM>,
    /// The virtual CPU.
    pub vcpu: VCpuRef,
}

impl VCpuTask {
    /// Create a new [`HvTask`].
    pub fn new(vm: &VMRef, vcpu: VCpuRef) -> Self {
        Self {
            vm: Arc::downgrade(vm),
            vcpu,
        }
    }

    /// Get a strong reference to the VM if it's still alive.
    /// Returns None if the VM has been dropped.
    pub fn vm(&self) -> VMRef {
        self.vm.upgrade().expect("VM has been dropped")
    }
}

#[extern_trait::extern_trait]
unsafe impl TaskExt for VCpuTask {}

pub trait AsVCpuTask {
    fn as_vcpu_task(&self) -> &VCpuTask;
}

impl AsVCpuTask for TaskInner {
    fn as_vcpu_task(&self) -> &VCpuTask {
        unsafe {
            self.task_ext()
                .expect("Not a VCpuTask")
                .downcast_ref::<VCpuTask>()
        }
    }
}
