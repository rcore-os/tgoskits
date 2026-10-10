//! Runtime gate sinks for events whose facts are produced outside this module.
//!
//! An event's enable state lives in its callback set, which the tracepoint
//! registry owns.  A layer that produces an event on its own hot path cannot
//! read that state, so it keeps a published copy and skips the record while no
//! consumer is attached.  The module that defines such an event registers the
//! publishing function of that layer here; the registry mirrors every
//! callback-set change to it, so adding an event stays local to the module
//! that defines it.

use alloc::vec::Vec;

use ax_tracepoint::TracePoint;

use super::KernelTraceAux;
use crate::sync::Mutex;

/// Publishes one event's enable state to the layer that produces it.
///
/// A sink runs inside the tracepoint update that changed the callback set, so
/// it must publish the state and return: it may not block, allocate without
/// bound, panic, or call back into tracepoint management (`update`, `register`
/// or `publish`), because neither the update lock nor the registry lock is
/// reentrant.  This module's own lock is not held while a sink runs.
pub(crate) type GateSink = fn(bool);

static GATE_SINKS: Mutex<Vec<(&'static TracePoint<KernelTraceAux>, GateSink)>> =
    Mutex::new(Vec::new());

/// Registers the gate sink of `tracepoint` and publishes its current state.
///
/// Registration happens during tracepoint initialization, before any consumer
/// can change a callback set; registering the same event twice is an invariant
/// violation.  Reading the current state under the same acquisition that
/// installs the entry keeps the initial publication from interleaving with a
/// concurrent callback-set change.
pub(crate) fn register(tracepoint: &'static TracePoint<KernelTraceAux>, sink: GateSink) {
    let enabled = {
        let mut sinks = GATE_SINKS.lock();
        assert!(
            !sinks
                .iter()
                .any(|(registered, _)| core::ptr::eq(*registered, tracepoint)),
            "gate sink already registered for this tracepoint"
        );
        sinks.push((tracepoint, sink));
        tracepoint.key_is_enabled()
    };
    sink(enabled);
}

/// Mirrors a callback-set change to the sink registered for `tracepoint`.
///
/// The registry calls this from the same update that changes the callback set,
/// so a tracefs `enable` write and a perf/BPF attach both reach the producing
/// layer.  The lookup holds this module's lock; the sink itself runs after the
/// guard is released.
pub(crate) fn publish(tracepoint: &'static TracePoint<KernelTraceAux>, enabled: bool) {
    let sink = {
        let sinks = GATE_SINKS.lock();
        sinks
            .iter()
            .find(|(registered, _)| core::ptr::eq(*registered, tracepoint))
            .map(|(_, sink)| *sink)
    };
    if let Some(sink) = sink {
        sink(enabled);
    }
}
