//! Architecture-independent interrupt line signaling.

use alloc::sync::Arc;

use ax_errno::{AxError, AxResult};
use axvm_types::{InterruptTriggerMode, IrqLineId};

/// Receives state changes and pulses from interrupt lines.
///
/// Implementations route the line operation to a VM-specific interrupt
/// controller backend.
pub trait IrqSink: Send + Sync {
    /// Sets whether a level-triggered interrupt line is asserted.
    fn set_level(&self, line: IrqLineId, asserted: bool) -> AxResult;

    /// Delivers one pulse from an edge-triggered interrupt line.
    fn pulse(&self, line: IrqLineId) -> AxResult;
}

/// A shareable interrupt line connected to an [`IrqSink`].
///
/// Clones share the same line identity, trigger mode, and sink.
#[derive(Clone)]
pub struct IrqLine(Arc<IrqLineInner>);

struct IrqLineInner {
    id: IrqLineId,
    trigger: InterruptTriggerMode,
    sink: Arc<dyn IrqSink>,
}

impl IrqLine {
    /// Creates an interrupt line connected to `sink`.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::sync::Arc;
    ///
    /// use axdevice_base::{AxResult, InterruptTriggerMode, IrqLine, IrqLineId, IrqSink};
    ///
    /// struct Sink;
    ///
    /// impl IrqSink for Sink {
    ///     fn set_level(&self, _line: IrqLineId, _asserted: bool) -> AxResult {
    ///         Ok(())
    ///     }
    ///
    ///     fn pulse(&self, _line: IrqLineId) -> AxResult {
    ///         Ok(())
    ///     }
    /// }
    ///
    /// let line = IrqLine::new(
    ///     IrqLineId(4),
    ///     InterruptTriggerMode::EdgeTriggered,
    ///     Arc::new(Sink),
    /// );
    /// line.pulse().unwrap();
    /// ```
    pub fn new(id: IrqLineId, trigger: InterruptTriggerMode, sink: Arc<dyn IrqSink>) -> Self {
        Self(Arc::new(IrqLineInner { id, trigger, sink }))
    }

    /// Asserts a level-triggered interrupt line.
    ///
    /// Returns [`AxError::InvalidInput`] for an edge-triggered line.
    pub fn raise(&self) -> AxResult {
        if self.0.trigger != InterruptTriggerMode::LevelTriggered {
            return Err(AxError::InvalidInput);
        }
        self.0.sink.set_level(self.0.id, true)
    }

    /// Deasserts a level-triggered interrupt line.
    ///
    /// Returns [`AxError::InvalidInput`] for an edge-triggered line.
    pub fn lower(&self) -> AxResult {
        if self.0.trigger != InterruptTriggerMode::LevelTriggered {
            return Err(AxError::InvalidInput);
        }
        self.0.sink.set_level(self.0.id, false)
    }

    /// Pulses an edge-triggered interrupt line.
    ///
    /// Returns [`AxError::InvalidInput`] for a level-triggered line.
    pub fn pulse(&self) -> AxResult {
        if self.0.trigger != InterruptTriggerMode::EdgeTriggered {
            return Err(AxError::InvalidInput);
        }
        self.0.sink.pulse(self.0.id)
    }
}
