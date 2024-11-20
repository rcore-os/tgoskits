use std::os::arceos::modules::axtask::def_task_ext;

use crate::vmm::{VCpuRef, VMRef};

/// Task extended data for the hypervisor.
pub struct TaskExt {
    /// The VM.
    pub vm: VMRef,
    /// The virtual memory address space.
    pub vcpu: VCpuRef,
}

impl TaskExt {
    pub const fn new(vm: VMRef, vcpu: VCpuRef) -> Self {
        Self { vm, vcpu }
    }
}

def_task_ext!(TaskExt);
