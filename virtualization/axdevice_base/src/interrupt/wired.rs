//! Wired interrupt inputs and per-device line connections.

use alloc::{string::String, sync::Arc};
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use axvm_types::InterruptTriggerMode;

use super::{ControllerInputId, InterruptControllerId, InterruptEndpoint, InterruptSourceId};

/// Errors reported while connecting or signaling interrupt endpoints.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum IrqError {
    /// The requested operation does not match the input trigger mode.
    #[error(
        "interrupt endpoint {endpoint:?} uses {actual:?} triggering, but {operation} requires \
         {expected:?} triggering"
    )]
    InvalidTriggerMode {
        /// The affected endpoint.
        endpoint: InterruptEndpoint,
        /// Operation requested by the caller.
        operation: &'static str,
        /// Trigger mode required by the operation.
        expected: InterruptTriggerMode,
        /// Trigger mode configured for the input.
        actual: InterruptTriggerMode,
    },
    /// An endpoint or request is invalid for the controller.
    #[error("invalid interrupt operation {operation} on {endpoint:?}: {detail}")]
    InvalidInput {
        /// The affected endpoint.
        endpoint: InterruptEndpoint,
        /// Operation rejected by the controller.
        operation: &'static str,
        /// Diagnostic detail describing the accepted input.
        detail: String,
    },
    /// The controller does not support the requested operation.
    #[error("unsupported interrupt operation {operation} on {endpoint:?}: {detail}")]
    Unsupported {
        /// The affected endpoint.
        endpoint: InterruptEndpoint,
        /// Unsupported operation.
        operation: &'static str,
        /// Diagnostic detail describing the limitation.
        detail: String,
    },
    /// The controller backend failed while handling an endpoint.
    #[error("interrupt backend operation {operation} failed for {endpoint:?}: {detail}")]
    Backend {
        /// The affected endpoint.
        endpoint: InterruptEndpoint,
        /// Backend operation that failed.
        operation: &'static str,
        /// Diagnostic detail from the backend.
        detail: String,
    },
}

/// Result type returned by virtual interrupt connection operations.
pub type IrqResult<T = ()> = Result<T, IrqError>;

/// Receives the aggregate electrical state of one controller input.
///
/// A [`WiredIrqInput`] performs per-source wired-OR accounting before invoking
/// this capability. Implementations therefore never need to distinguish the
/// individual devices sharing an input.
pub trait WiredIrqSink: Send + Sync {
    /// Updates the aggregate state of a level-triggered controller input.
    fn set_level(&self, input: ControllerInputId, asserted: bool) -> IrqResult;

    /// Delivers one edge to a controller input.
    fn pulse(&self, input: ControllerInputId) -> IrqResult;
}

/// A controller-owned wired interrupt input.
///
/// Calling [`Self::connect`] creates an independently identified device
/// source. Level-triggered sources are combined using wired-OR semantics.
#[derive(Clone)]
pub struct WiredIrqInput(Arc<WiredIrqInputInner>);

struct WiredIrqInputInner {
    controller: InterruptControllerId,
    input: ControllerInputId,
    trigger: InterruptTriggerMode,
    next_source: AtomicU64,
    asserted_sources: AtomicUsize,
    level_transition: AtomicBool,
    sink: Arc<dyn WiredIrqSink>,
}

impl WiredIrqInput {
    /// Creates a controller input backed by `sink`.
    ///
    /// Controller implementations should retain and reuse one instance for
    /// each physical input so that all connected sources share level state.
    pub fn new(
        controller: InterruptControllerId,
        input: ControllerInputId,
        trigger: InterruptTriggerMode,
        sink: Arc<dyn WiredIrqSink>,
    ) -> Self {
        Self(Arc::new(WiredIrqInputInner {
            controller,
            input,
            trigger,
            next_source: AtomicU64::new(0),
            asserted_sources: AtomicUsize::new(0),
            level_transition: AtomicBool::new(false),
            sink,
        }))
    }

    /// Creates a device connection to this input.
    ///
    /// # Errors
    ///
    /// Returns [`IrqError::InvalidInput`] if the source identifier space has
    /// been exhausted.
    pub fn connect(&self) -> IrqResult<IrqLine> {
        let source = self
            .0
            .next_source
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                current.checked_add(1)
            })
            .map(InterruptSourceId::new)
            .map_err(|_| IrqError::InvalidInput {
                endpoint: self.endpoint(),
                operation: "connect interrupt source",
                detail: "the input source identifier space is exhausted".into(),
            })?;

        Ok(IrqLine(Arc::new(IrqLineInner {
            input: self.clone(),
            source,
            asserted: AtomicBool::new(false),
            level_transition: AtomicBool::new(false),
        })))
    }

    /// Returns the controller that owns this input.
    pub fn controller(&self) -> InterruptControllerId {
        self.0.controller
    }

    /// Returns the controller-local input number.
    pub fn input(&self) -> ControllerInputId {
        self.0.input
    }

    /// Returns the configured trigger mode.
    pub fn trigger(&self) -> InterruptTriggerMode {
        self.0.trigger
    }

    fn endpoint(&self) -> InterruptEndpoint {
        InterruptEndpoint::Wired {
            controller: self.controller(),
            input: self.input(),
        }
    }

    fn raise_source(&self) -> IrqResult {
        let _transition = SerialTransition::acquire(&self.0.level_transition);
        let previous = self.0.asserted_sources.load(Ordering::Relaxed);
        let asserted = previous
            .checked_add(1)
            .ok_or_else(|| IrqError::InvalidInput {
                endpoint: self.endpoint(),
                operation: "raise interrupt source",
                detail: "the asserted source count is exhausted".into(),
            })?;
        self.0.asserted_sources.store(asserted, Ordering::Relaxed);
        if previous != 0 {
            return Ok(());
        }
        if let Err(error) = self.0.sink.set_level(self.input(), true) {
            self.0.asserted_sources.store(previous, Ordering::Relaxed);
            return Err(error);
        }
        Ok(())
    }

    fn lower_source(&self) -> IrqResult {
        let _transition = SerialTransition::acquire(&self.0.level_transition);
        let previous = self.0.asserted_sources.load(Ordering::Relaxed);
        if previous == 0 {
            return Err(IrqError::InvalidInput {
                endpoint: self.endpoint(),
                operation: "lower interrupt source",
                detail: "the interrupt source is not asserted".into(),
            });
        }
        let asserted = previous - 1;
        self.0.asserted_sources.store(asserted, Ordering::Relaxed);
        if asserted != 0 {
            return Ok(());
        }
        if let Err(error) = self.0.sink.set_level(self.input(), false) {
            self.0.asserted_sources.store(previous, Ordering::Relaxed);
            return Err(error);
        }
        Ok(())
    }

    fn disconnect_asserted_source(&self) {
        let _transition = SerialTransition::acquire(&self.0.level_transition);
        let previous = self.0.asserted_sources.load(Ordering::Relaxed);
        if previous == 0 {
            return;
        }
        let asserted = previous - 1;
        self.0.asserted_sources.store(asserted, Ordering::Relaxed);
        if asserted == 0 {
            let _ = self.0.sink.set_level(self.input(), false);
        }
    }
}

impl core::fmt::Debug for WiredIrqInput {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("WiredIrqInput")
            .field("controller", &self.controller())
            .field("input", &self.input())
            .field("trigger", &self.trigger())
            .field(
                "asserted_sources",
                &self.0.asserted_sources.load(Ordering::Relaxed),
            )
            .finish_non_exhaustive()
    }
}

/// A shareable device connection to one wired controller input.
///
/// Clones refer to the same source. A separately connected device receives a
/// different [`InterruptSourceId`], even when both devices share an input.
#[derive(Clone)]
pub struct IrqLine(Arc<IrqLineInner>);

struct IrqLineInner {
    input: WiredIrqInput,
    source: InterruptSourceId,
    asserted: AtomicBool,
    level_transition: AtomicBool,
}

impl IrqLine {
    /// Asserts this level-triggered source.
    pub fn raise(&self) -> IrqResult {
        self.require_trigger("raise", InterruptTriggerMode::LevelTriggered)?;
        let _transition = SerialTransition::acquire(&self.0.level_transition);
        if self.0.asserted.load(Ordering::Relaxed) {
            return Ok(());
        }
        self.0.input.raise_source()?;
        self.0.asserted.store(true, Ordering::Relaxed);
        Ok(())
    }

    /// Deasserts this level-triggered source.
    pub fn lower(&self) -> IrqResult {
        self.require_trigger("lower", InterruptTriggerMode::LevelTriggered)?;
        let _transition = SerialTransition::acquire(&self.0.level_transition);
        if !self.0.asserted.load(Ordering::Relaxed) {
            return Ok(());
        }
        self.0.input.lower_source()?;
        self.0.asserted.store(false, Ordering::Relaxed);
        Ok(())
    }

    /// Delivers one edge from this source.
    pub fn pulse(&self) -> IrqResult {
        self.require_trigger("pulse", InterruptTriggerMode::EdgeTriggered)?;
        self.0.input.0.sink.pulse(self.input())
    }

    /// Returns the owning controller.
    pub fn controller(&self) -> InterruptControllerId {
        self.0.input.controller()
    }

    /// Returns the controller-local input number.
    pub fn input(&self) -> ControllerInputId {
        self.0.input.input()
    }

    /// Returns this connection's source identifier.
    pub fn source(&self) -> InterruptSourceId {
        self.0.source
    }

    /// Returns whether both handles refer to the same connected source.
    pub fn same_connection(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    /// Returns the configured trigger mode.
    pub fn trigger(&self) -> InterruptTriggerMode {
        self.0.input.trigger()
    }

    fn require_trigger(
        &self,
        operation: &'static str,
        expected: InterruptTriggerMode,
    ) -> IrqResult {
        let actual = self.trigger();
        if actual != expected {
            return Err(IrqError::InvalidTriggerMode {
                endpoint: self.0.input.endpoint(),
                operation,
                expected,
                actual,
            });
        }
        Ok(())
    }
}

impl core::fmt::Debug for IrqLine {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("IrqLine")
            .field("controller", &self.controller())
            .field("input", &self.input())
            .field("source", &self.source())
            .field("trigger", &self.trigger())
            .field("asserted", &self.0.asserted.load(Ordering::Relaxed))
            .finish()
    }
}

impl Drop for IrqLineInner {
    fn drop(&mut self) {
        if self.asserted.load(Ordering::Relaxed) {
            self.input.disconnect_asserted_source();
        }
    }
}

/// Serializes one short electrical transition without requiring an allocator
/// or a scheduler-aware lock in this reusable `no_std` crate.
struct SerialTransition<'a> {
    flag: &'a AtomicBool,
}

impl<'a> SerialTransition<'a> {
    fn acquire(flag: &'a AtomicBool) -> Self {
        while flag
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            while flag.load(Ordering::Acquire) {
                core::hint::spin_loop();
            }
        }
        Self { flag }
    }
}

impl Drop for SerialTransition<'_> {
    fn drop(&mut self) {
        self.flag.store(false, Ordering::Release);
    }
}
