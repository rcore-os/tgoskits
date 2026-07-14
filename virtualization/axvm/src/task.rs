//! Host task extension data used by AxVM vCPU threads.

extern crate alloc;

use alloc::{boxed::Box, sync::Arc};

use ax_std::os::arceos::task::{ThreadExtension, ThreadExtensionOps, ThreadId};

use crate::{
    host::task::CurrentTask,
    vm::{AxVCpuRef, AxVMRef},
};

/// Task extension owned by one vCPU host thread.
pub struct VCpuTask {
    /// The VM. Stored weakly to avoid keeping a VM alive through its task.
    pub vm: alloc::sync::Weak<crate::AxVM>,
    /// The virtual CPU.
    pub vcpu: AxVCpuRef,
}

impl VCpuTask {
    /// Creates the extension for one VM vCPU.
    pub fn new(vm: &AxVMRef, vcpu: AxVCpuRef) -> Self {
        Self {
            vm: Arc::downgrade(vm),
            vcpu,
        }
    }

    /// Gets a strong reference to the VM.
    ///
    /// # Panics
    ///
    /// Panics if the VM has already been dropped.
    pub fn vm(&self) -> AxVMRef {
        self.vm.upgrade().expect("VM has been dropped")
    }
}

/// Accesses the vCPU extension of the current host thread.
pub trait AsVCpuTask {
    /// Returns this thread's vCPU extension, if present.
    fn try_as_vcpu_task(&self) -> Option<&VCpuTask>;

    /// Returns this thread's vCPU extension.
    ///
    /// # Panics
    ///
    /// Panics when the current host thread is not a vCPU thread.
    fn as_vcpu_task(&self) -> &VCpuTask;
}

impl AsVCpuTask for CurrentTask {
    fn try_as_vcpu_task(&self) -> Option<&VCpuTask> {
        let extension = self.extension()?;
        if !core::ptr::eq(extension.ops(), &VCPU_TASK_EXTENSION_OPS) {
            return None;
        }
        // SAFETY: the callback-table identity is private to `VCpuTask`, and the
        // current task handle keeps the scheduler header live for this borrow.
        Some(unsafe { &*(extension.data() as *const VCpuTask) })
    }

    fn as_vcpu_task(&self) -> &VCpuTask {
        self.try_as_vcpu_task().expect("not a vCPU host thread")
    }
}

pub(crate) fn into_thread_extension(task: VCpuTask) -> ThreadExtension {
    let data = Box::into_raw(Box::new(task)) as usize;
    // SAFETY: `VCPU_TASK_EXTENSION_OPS` is the unique owner of this Box pointer
    // after it is transferred into the runtime thread extension.
    unsafe { ThreadExtension::new(data, &VCPU_TASK_EXTENSION_OPS) }
}

static VCPU_TASK_EXTENSION_OPS: ThreadExtensionOps = ThreadExtensionOps {
    on_switch_in: vcpu_task_hook,
    on_switch_out: vcpu_task_switch_out,
    on_exit: vcpu_task_hook,
    on_deadline_overrun: vcpu_task_hook,
    drop: drop_vcpu_task,
};

unsafe extern "Rust" fn vcpu_task_hook(_data: usize, _thread: ThreadId) {}

unsafe extern "Rust" fn vcpu_task_switch_out(
    _data: usize,
    _thread: ThreadId,
    _reason: ax_std::os::arceos::task::SwitchReason,
) {
}

unsafe extern "Rust" fn drop_vcpu_task(data: usize) {
    // SAFETY: `into_thread_extension` transfers one unique Box to this callback
    // table, and the ax-runtime outer extension forwards its final release once.
    drop(unsafe { Box::from_raw(data as *mut VCpuTask) });
}
