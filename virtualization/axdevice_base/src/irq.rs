//! Architecture-independent interrupt line signaling.

use alloc::{string::String, sync::Arc};

use axvm_types::{InterruptTriggerMode, IrqLineId};

/// Errors reported while routing or signaling a virtual interrupt line.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum IrqError {
    /// The requested operation does not match the line's trigger mode.
    #[error(
        "IRQ line {line:?} uses {actual:?} triggering, but {operation} requires {expected:?} \
         triggering"
    )]
    InvalidTriggerMode {
        /// The affected interrupt line.
        line: IrqLineId,
        /// The operation that was requested.
        operation: &'static str,
        /// The trigger mode required by the operation.
        expected: InterruptTriggerMode,
        /// The trigger mode configured for the line.
        actual: InterruptTriggerMode,
    },
    /// The interrupt line identifier is invalid for the sink.
    #[error("invalid IRQ line {line:?} during {operation}: {detail}")]
    InvalidLine {
        /// The rejected interrupt line.
        line: IrqLineId,
        /// The operation that rejected the line.
        operation: &'static str,
        /// Diagnostic detail describing the valid range or assignment.
        detail: String,
    },
    /// The interrupt sink does not support the requested operation.
    #[error("unsupported IRQ operation {operation} on line {line:?}: {detail}")]
    Unsupported {
        /// The affected interrupt line.
        line: IrqLineId,
        /// The unsupported operation.
        operation: &'static str,
        /// Diagnostic detail describing the limitation.
        detail: String,
    },
    /// The interrupt controller backend failed.
    #[error("IRQ backend operation {operation} failed for line {line:?}: {detail}")]
    Backend {
        /// The affected interrupt line.
        line: IrqLineId,
        /// The backend operation that failed.
        operation: &'static str,
        /// Diagnostic detail from the backend.
        detail: String,
    },
}

/// Result type returned by virtual interrupt routing operations.
pub type IrqResult<T = ()> = Result<T, IrqError>;

/// Receives state changes and pulses from interrupt lines.
///
/// Implementations route the line operation to a VM-specific interrupt
/// controller backend.
pub trait IrqSink: Send + Sync {
    /// Sets whether a level-triggered interrupt line is asserted.
    fn set_level(&self, line: IrqLineId, asserted: bool) -> IrqResult;

    /// Delivers one pulse from an edge-triggered interrupt line.
    fn pulse(&self, line: IrqLineId) -> IrqResult;
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
    /// use axdevice_base::{InterruptTriggerMode, IrqLine, IrqLineId, IrqResult, IrqSink};
    ///
    /// struct Sink;
    ///
    /// impl IrqSink for Sink {
    ///     fn set_level(&self, _line: IrqLineId, _asserted: bool) -> IrqResult {
    ///         Ok(())
    ///     }
    ///
    ///     fn pulse(&self, _line: IrqLineId) -> IrqResult {
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
    /// Returns [`IrqError::InvalidTriggerMode`] for an edge-triggered line.
    pub fn raise(&self) -> IrqResult {
        if self.0.trigger != InterruptTriggerMode::LevelTriggered {
            return Err(IrqError::InvalidTriggerMode {
                line: self.0.id,
                operation: "raise",
                expected: InterruptTriggerMode::LevelTriggered,
                actual: self.0.trigger,
            });
        }
        self.0.sink.set_level(self.0.id, true)
    }

    /// Deasserts a level-triggered interrupt line.
    ///
    /// Returns [`IrqError::InvalidTriggerMode`] for an edge-triggered line.
    pub fn lower(&self) -> IrqResult {
        if self.0.trigger != InterruptTriggerMode::LevelTriggered {
            return Err(IrqError::InvalidTriggerMode {
                line: self.0.id,
                operation: "lower",
                expected: InterruptTriggerMode::LevelTriggered,
                actual: self.0.trigger,
            });
        }
        self.0.sink.set_level(self.0.id, false)
    }

    /// Pulses an edge-triggered interrupt line.
    ///
    /// Returns [`IrqError::InvalidTriggerMode`] for a level-triggered line.
    pub fn pulse(&self) -> IrqResult {
        if self.0.trigger != InterruptTriggerMode::EdgeTriggered {
            return Err(IrqError::InvalidTriggerMode {
                line: self.0.id,
                operation: "pulse",
                expected: InterruptTriggerMode::EdgeTriggered,
                actual: self.0.trigger,
            });
        }
        self.0.sink.pulse(self.0.id)
    }
}
