use core::ptr::NonNull;

use cpu_local::ExecutionContextHeader;

use crate::KernelTlsBase;

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
    /// Creates empty task-local switch state.
    pub const fn new() -> Self {
        Self {
            context_header: 0,
            kernel_tls: KernelTlsBase::new(0),
        }
    }

    /// Configures the task-owned TLS base for the selected image mode.
    pub(crate) fn set_kernel_tls(&mut self, kernel_tls: KernelTlsBase) {
        self.kernel_tls = KernelTlsBase::for_task_context(kernel_tls);
    }

    /// Sets the stable task-owned execution-context header.
    pub fn set_context_header(&mut self, header: NonNull<ExecutionContextHeader>) {
        self.context_header = header.as_ptr() as usize;
    }

    /// Returns the configured task-owned execution-context header.
    pub const fn context_header(&self) -> Option<NonNull<ExecutionContextHeader>> {
        NonNull::new(self.context_header as *mut ExecutionContextHeader)
    }
}

const _: () = {
    assert!(core::mem::size_of::<TaskLocalState>() == 2 * core::mem::size_of::<usize>());
    assert!(core::mem::align_of::<TaskLocalState>() == core::mem::align_of::<usize>());
};
