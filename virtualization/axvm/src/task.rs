//! Host task extension data used by AxVM vCPU tasks.

extern crate alloc;

use alloc::{
    boxed::Box,
    sync::{Arc, Weak},
};
use core::ptr;

use crate::{
    host::task::{
        SwitchReason, TaskHandle, ThreadExtension, ThreadExtensionOps, ThreadId, task_extension,
    },
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

    /// Transfers this vCPU attachment into the runtime scheduler extension.
    pub(crate) fn into_thread_extension(self) -> ThreadExtension {
        let data = Box::into_raw(Box::new(self)) as usize;
        // SAFETY: `data` is one uniquely owned `Box<VCpuTask>`. The runtime
        // invokes `drop_vcpu_task` exactly once after all scheduler callbacks
        // and strong handles have retired.
        unsafe { ThreadExtension::new(data, &VCPU_TASK_EXTENSION_OPS) }
    }
}

static VCPU_TASK_EXTENSION_OPS: ThreadExtensionOps = ThreadExtensionOps {
    on_switch_in: vcpu_task_switch_in,
    on_switch_out: vcpu_task_switch_out,
    on_exit: vcpu_task_exit,
    on_deadline_overrun: vcpu_task_deadline_overrun,
    drop: drop_vcpu_task,
};

unsafe extern "Rust" fn vcpu_task_switch_in(_data: usize, _thread: ThreadId) {}

unsafe extern "Rust" fn vcpu_task_switch_out(
    _data: usize,
    _thread: ThreadId,
    _reason: SwitchReason,
) {
}

unsafe extern "Rust" fn vcpu_task_exit(_data: usize, _thread: ThreadId) {}

unsafe extern "Rust" fn vcpu_task_deadline_overrun(_data: usize, _thread: ThreadId) {}

unsafe extern "Rust" fn drop_vcpu_task(data: usize) {
    // SAFETY: `into_thread_extension` transferred exactly this box to the
    // runtime, and the extension drop callback is its sole destructor.
    drop(unsafe { Box::from_raw(data as *mut VCpuTask) });
}

/// Access a vCPU task extension from an ArceOS task.
pub trait AsVCpuTask {
    /// Return this task's vCPU extension if it has one.
    fn try_as_vcpu_task(&self) -> Option<&VCpuTask>;

    /// Return this task's vCPU extension.
    fn as_vcpu_task(&self) -> &VCpuTask;
}

impl AsVCpuTask for TaskHandle {
    fn try_as_vcpu_task(&self) -> Option<&VCpuTask> {
        let extension = task_extension(self)
            .unwrap_or_else(|error| panic!("failed to inspect AxVM task extension: {error}"))?;
        if !ptr::eq(extension.ops(), &VCPU_TASK_EXTENSION_OPS) {
            return None;
        }
        let data = extension.data();
        if data == 0 || !data.is_multiple_of(core::mem::align_of::<VCpuTask>()) {
            panic!("AxVM task extension contains an invalid data pointer");
        }
        // SAFETY: the callback-table identity and pointer layout were checked
        // above. `self` is a strong scheduler handle, so the runtime cannot
        // invoke the extension drop callback during the returned borrow.
        Some(unsafe { &*(data as *const VCpuTask) })
    }

    fn as_vcpu_task(&self) -> &VCpuTask {
        self.try_as_vcpu_task().expect("Not a VCpuTask")
    }
}
