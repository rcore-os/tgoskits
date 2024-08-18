use axvm::{AxVCpuRef, AxVMRef};

use crate::hal::AxVMHalImpl;

/// Task extended data for the monolithic kernel.
pub struct TaskExt {
    /// The VM.
    pub vm: AxVMRef<AxVMHalImpl>,
    /// The virtual memory address space.
    pub vcpu: AxVCpuRef<AxVMHalImpl>,
}

impl TaskExt {
    pub const fn new(vm: AxVMRef<AxVMHalImpl>, vcpu: AxVCpuRef<AxVMHalImpl>) -> Self {
        Self { vm, vcpu }
    }
}

axtask::def_task_ext!(TaskExt);
