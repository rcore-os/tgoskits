//! Useful synchronization primitives.

#[doc(no_inline)]
pub use core::sync::atomic;

#[cfg(feature = "alloc")]
#[doc(no_inline)]
pub use alloc_crate::sync::{Arc, Weak};
pub use ax_kspin::{dump_lockdep_trace, set_lockdep_trace_enabled};

#[cfg(feature = "multitask")]
mod mutex;

#[cfg(not(feature = "multitask"))]
#[cfg_attr(doc, doc(cfg(not(feature = "multitask"))))]
pub use ax_kspin::{SpinRaw as Mutex, SpinRawGuard as MutexGuard};

#[cfg(feature = "multitask")]
#[cfg_attr(doc, doc(cfg(feature = "multitask")))]
pub use self::mutex::{Mutex, MutexGuard}; // never used in IRQ context
