//! Starry-owned synchronization and publication for tracepoint runtime state.

use alloc::sync::Arc;

use ax_tracepoint::{ExtTracePoint, TracePoint};

use super::KernelTraceAux;
use crate::sync::NoPreemptMutex;

/// One Starry tracepoint runtime state guarded for scheduler and IRQ readers.
///
/// The wrapper is the only mutation boundary: it updates the callback gate
/// while holding the same non-preemptible lock that protects the callback
/// list. This preserves Starry's existing callback execution context while
/// replacing ax-tracepoint's former live kernel-text patching.
#[derive(Clone)]
pub struct KernelExtTracePoint {
    state: Arc<NoPreemptMutex<ExtTracePoint<KernelTraceAux>>>,
}

impl KernelExtTracePoint {
    pub(super) fn new(state: ExtTracePoint<KernelTraceAux>) -> Self {
        Self {
            state: Arc::new(NoPreemptMutex::new(state)),
        }
    }

    /// Runs one read while holding the scheduler-safe state lock.
    pub fn read<R>(&self, operation: impl FnOnce(&ExtTracePoint<KernelTraceAux>) -> R) -> R {
        operation(&self.state.lock())
    }

    /// Applies one state update and publishes the resulting callback gate.
    pub fn update<R>(&self, operation: impl FnOnce(&mut ExtTracePoint<KernelTraceAux>) -> R) -> R {
        let mut state = self.state.lock();
        let result = operation(&mut state);
        state.trace_point().set_callback_gate(state.has_callbacks());
        result
    }

    /// Returns the immutable static descriptor associated with this state.
    pub fn trace_point(&self) -> &'static TracePoint<KernelTraceAux> {
        self.read(ExtTracePoint::trace_point)
    }
}
