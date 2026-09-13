//! OS-independent lockdep diagnostic bridge.

/// Enables or disables runtime lockdep trace recording.
pub fn set_lockdep_trace_enabled(enabled: bool) {
    crate::interface::set_trace_enabled(enabled);
}

/// Dumps the runtime lockdep trace buffer.
pub fn dump_lockdep_trace() {
    crate::interface::dump_trace();
}
