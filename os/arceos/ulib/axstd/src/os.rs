//! OS-specific functionality.

/// ArceOS-specific definitions.
///
/// `api` re-exports the public ArceOS API surface. Prefer this entry for
/// ArceOS-specific operations that do not have a std-like wrapper.
///
/// `modules` re-exports lower-level ArceOS modules as an escape hatch for
/// complex systems such as Axvisor. Ordinary applications should prefer
/// `ax_std::{fs, io, thread, sync, time, net}`.
pub mod arceos {
    /// ArceOS public API facade.
    pub use ax_api as api;

    /// Guards for ArceOS interrupt and preemption contexts.
    pub mod guard {
        pub use ax_runtime::task::sync::{IrqSaveGuard, PreemptGuard, PreemptIrqSaveGuard};
    }

    /// Lower-level ArceOS module facade for system components.
    #[doc(no_inline)]
    pub use ax_api::modules;
    /// ArceOS host driver registry and firmware discovery capabilities.
    #[doc(no_inline)]
    pub use ax_driver as driver;
    /// ArceOS per-CPU storage and CPU-pinning capabilities.
    #[doc(no_inline)]
    pub use ax_percpu as percpu;

    /// Task and native synchronization for ArceOS kernel contexts.
    pub mod sync {
        pub use ax_runtime::task::sync::*;
    }

    /// OS-independent task scheduler types and ArceOS runtime operations.
    pub use ax_runtime::{diagnostics, irq, task, thread};
}

#[cfg(feature = "std-compat")]
pub mod libc_compat;

#[cfg(any(feature = "std-compat", all(test, feature = "host-test")))]
mod futex;
