//! Machine context for the current CPU architecture.

#[cfg(feature = "context")]
pub use crate::arch::current::context::TaskContext;
pub use crate::arch::current::{context::TrapFrame as UserRegisters, trap::KernelTrapFrame};

/// Opaque task address installed in the architecture's current-task register.
///
/// The runtime owns the pointed-to object and its layout. Creating this value
/// does not make the object readable or extend its lifetime. A context switch
/// requires the runtime to keep the anchor pinned and alive through its use.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TaskAnchor(core::ptr::NonNull<()>);

impl TaskAnchor {
    /// Erases the runtime object type without dereferencing its address.
    pub const fn new<T>(pointer: core::ptr::NonNull<T>) -> Self {
        Self(pointer.cast())
    }

    /// Returns the opaque runtime address.
    pub const fn as_ptr(self) -> *mut () {
        self.0.as_ptr()
    }
}

/// Kernel task-local storage base owned by one execution context.
///
/// This value follows a task across CPUs. It must never be used as a CPU-local
/// anchor or initialized from an architecture per-CPU register.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct KernelTlsBase(usize);

impl KernelTlsBase {
    /// Creates a kernel TLS base from its virtual address.
    pub const fn new(address: usize) -> Self {
        Self(address)
    }

    /// Returns the virtual address represented by this TLS base.
    pub const fn as_usize(self) -> usize {
        self.0
    }

    #[cfg(feature = "context")]
    pub(crate) fn for_task_context(requested: Self) -> Self {
        if cfg!(kernel_tls) {
            requested
        } else {
            assert!(
                requested.0 == 0,
                "LinuxCurrent task contexts must not own a kernel TLS register"
            );
            Self(0)
        }
    }
}
