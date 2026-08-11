//! Runtime-facing lockdep diagnostics owned by ax-task.

use core::{
    panic::Location,
    sync::atomic::{AtomicPtr, AtomicU32},
};

/// Borrowed lock-class storage from an external fixed-layout wrapper.
#[derive(Clone, Copy)]
pub struct LockClass<'lock> {
    pub class_id: &'lock AtomicU32,
    pub class_key: &'lock AtomicPtr<Location<'static>>,
}

/// Enables or disables native lockdep trace recording.
pub fn set_lockdep_trace_enabled(enabled: bool) {
    #[cfg(feature = "lockdep")]
    crate::sync::lockdep::set_lockdep_trace_enabled(enabled);

    #[cfg(not(feature = "lockdep"))]
    let _ = enabled;
}

/// Dumps the native lockdep trace buffer.
pub fn dump_lockdep_trace() {
    #[cfg(feature = "lockdep")]
    crate::sync::lockdep::dump_lockdep_trace();
}
