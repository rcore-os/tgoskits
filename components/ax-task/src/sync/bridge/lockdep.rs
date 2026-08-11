//! Runtime-facing lockdep diagnostics owned by ax-task.

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
