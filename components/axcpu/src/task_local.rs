use core::ptr::NonNull;

use crate::{KernelTlsBase, context::TaskAnchor};

/// Architecture-neutral task state participating in the final switch tail.
///
/// Architecture switch tails consume the current-header pointer whenever the
/// hardware provides a task register independent of kernel TLS. Backends whose
/// task register is also the TLS base use the CPU runtime anchor for current.
/// Keeping both values adjacent centralizes their switch-time ownership.
#[repr(C)]
#[derive(Debug, Default)]
pub struct TaskLocalState {
    pub(crate) context_header: usize,
    pub(crate) kernel_tls: KernelTlsBase,
}

impl TaskLocalState {
    /// Configures the task-owned TLS base for the selected image mode.
    pub(crate) fn set_kernel_tls(&mut self, kernel_tls: KernelTlsBase) {
        self.kernel_tls = KernelTlsBase::for_task_context(kernel_tls);
    }

    /// Sets the stable task-owned runtime task anchor.
    pub fn set_task_anchor(&mut self, header: TaskAnchor) {
        self.context_header = header.as_ptr() as usize;
    }

    /// Returns the configured task-owned runtime task anchor.
    pub const fn task_anchor(&self) -> Option<TaskAnchor> {
        match NonNull::new(self.context_header as *mut ()) {
            Some(pointer) => Some(TaskAnchor::new(pointer)),
            None => None,
        }
    }
}

const _: () = {
    assert!(core::mem::size_of::<TaskLocalState>() == 2 * core::mem::size_of::<usize>());
    assert!(core::mem::align_of::<TaskLocalState>() == core::mem::align_of::<usize>());
};
