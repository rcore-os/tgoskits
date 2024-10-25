use std::os::arceos::modules::axtask::def_task_ext;

use axvm::{AxVCpuRef, AxVMRef};

use crate::hal::AxVMHalImpl;

/// Task extended data for the hypervisor.
pub struct TaskExt {
    /// The VM.
    pub vm: AxVMRef<AxVMHalImpl>,
    /// The virtual memory address space.
    pub vcpu: AxVCpuRef,
}

impl TaskExt {
    pub const fn new(vm: AxVMRef<AxVMHalImpl>, vcpu: AxVCpuRef) -> Self {
        Self { vm, vcpu }
    }
}

def_task_ext!(TaskExt);
