use core::ptr::NonNull;

use cpu_local::CurrentThreadHeader;

use crate::KernelTlsBase;

/// Architecture-neutral task state participating in the final switch tail.
///
/// The current-header pointer is consumed only by LinuxCurrent assembly. The
/// kernel TLS base is consumed only by TLS-enabled unikernel assembly. Keeping
/// both in one C-layout component centralizes their adjacency and ownership.
#[repr(C)]
#[derive(Debug, Default)]
pub struct TaskLocalState {
    pub(crate) current_header: usize,
    pub(crate) kernel_tls: KernelTlsBase,
}

impl TaskLocalState {
    /// Creates empty task-local switch state.
    pub const fn new() -> Self {
        Self {
            current_header: 0,
            kernel_tls: KernelTlsBase::new(0),
        }
    }

    /// Configures the task-owned TLS base for the selected image mode.
    pub(crate) fn set_kernel_tls(&mut self, kernel_tls: KernelTlsBase) {
        self.kernel_tls = KernelTlsBase::for_task_context(kernel_tls);
    }

    /// Sets the stable task-owned current-thread header.
    pub fn set_current_header(&mut self, header: NonNull<CurrentThreadHeader>) {
        self.current_header = header.as_ptr() as usize;
    }

    /// Returns the configured task-owned current-thread header.
    pub const fn current_header(&self) -> Option<NonNull<CurrentThreadHeader>> {
        NonNull::new(self.current_header as *mut CurrentThreadHeader)
    }
}

const _: () = {
    assert!(core::mem::size_of::<TaskLocalState>() == 2 * core::mem::size_of::<usize>());
    assert!(core::mem::align_of::<TaskLocalState>() == core::mem::align_of::<usize>());
};
