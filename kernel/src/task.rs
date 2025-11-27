use crate::vmm::{VCpuRef, VM, VMRef};
use alloc::sync::{Arc, Weak};
use std::os::arceos::modules::axtask::def_task_ext;

/// Task extended data for the hypervisor.
pub struct TaskExt {
    /// The VM (Weak reference to avoid keeping VM alive).
    pub vm: Weak<VM>,
    /// The virtual CPU.
    pub vcpu: VCpuRef,
}

impl TaskExt {
    /// Create TaskExt with a Weak reference from a VMRef
    pub const fn new(vm: Weak<VM>, vcpu: VCpuRef) -> Self {
        Self { vm, vcpu }
    }

    /// Get a strong reference to the VM if it's still alive.
    /// Returns None if the VM has been dropped.
    pub fn vm(&self) -> VMRef {
        self.vm.upgrade().expect("VM has been dropped")
    }

    /// Helper to create TaskExt from a VMRef by downgrading to Weak.
    pub fn from_vm_ref(vm: VMRef, vcpu: VCpuRef) -> Self {
        Self {
            vm: Arc::downgrade(&vm),
            vcpu,
        }
    }
}

def_task_ext!(TaskExt);
