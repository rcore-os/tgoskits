//! Useful synchronization primitives.

#[cfg(feature = "alloc")]
#[doc(no_inline)]
pub use alloc::sync::{Arc, Weak};
#[doc(no_inline)]
pub use core::sync::atomic;

pub use ax_runtime::sync::{dump_lockdep_trace, set_lockdep_trace_enabled};

mod mutex;

pub use self::mutex::{Mutex, MutexGuard}; // never used in IRQ context
