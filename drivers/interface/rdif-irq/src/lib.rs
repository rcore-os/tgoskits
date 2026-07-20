//! Portable boundary between device IRQ capture and an OS runtime.
//!
//! An endpoint turns destructive device status into an owned, stable memory
//! event. Scheduling, workqueues, wakers, and IRQ-controller policy belong to
//! the OS glue that consumes that event.

#![no_std]

use core::{error::Error, num::NonZeroU64};

/// Captures stable device events from one interrupt source.
///
/// Implementations are normally moved into an OS IRQ registration and called
/// non-reentrantly. [`Self::capture`] must be bounded and must not allocate,
/// block, call arbitrary callbacks, or request a scheduling policy.
pub trait IrqEndpoint: Send + 'static {
    /// Stable event published after required device acknowledgement.
    ///
    /// The value is `Copy` so every hard-IRQ branch can move or discard it
    /// without running a destructor or freeing hidden ownership.
    type Event: Copy + Send + 'static;

    /// Device-specific reason why capture could not safely continue.
    ///
    /// Faults obey the same no-destructor rule as events; rich diagnostics are
    /// assembled later by the maintenance owner.
    type Fault: Copy + Error + Send + Sync + 'static;

    /// Reads and acknowledges one device interrupt source.
    fn capture(&mut self) -> IrqCapture<Self::Event, Self::Fault>;

    /// Stops this endpoint's exact device interrupt source after publication
    /// or capture can no longer make forward progress.
    ///
    /// This operation must be bounded, allocation-free, non-blocking, and
    /// safe in hard-IRQ context. It must be idempotent for the same device
    /// epoch because an OS runtime may retry containment while closing an
    /// overflowing event channel. Success returns the exact source token that
    /// remains device-masked; failure means the runtime must treat the source
    /// as uncontained and mask its parent action or interrupt line.
    fn contain(&mut self, cause: ContainmentCause) -> Result<MaskedSource, Self::Fault>;
}

/// Runtime condition that requires fail-closed device-source masking.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContainmentCause {
    /// The destination was closed during IRQ event publication.
    PublicationClosed,
    /// The bounded destination had no capacity for another captured event.
    PublicationFull,
    /// No live owner-side worker can consume and eventually rearm the source.
    OwnerUnavailable,
    /// Capture itself reported a device-specific fault without containment.
    CaptureFault,
}

/// Owner-side capability that reopens a device source masked during capture.
///
/// This endpoint is intentionally separate from [`IrqEndpoint`]. The capture
/// endpoint normally belongs to the OS hard-IRQ action while this control
/// endpoint belongs to the bounded worker that consumed the captured event.
/// Implementations must compare [`MaskedSource::generation`] with their active
/// device epoch before changing hardware state. Stale, replayed, or partially
/// overlapping tokens must return an error and leave the source masked.
pub trait IrqSourceControl: Send + 'static {
    /// Driver-specific, matchable failure reported by a rearm attempt.
    type Error: Error + Send + Sync + 'static;

    /// Rearms exactly the source bits and generation named by `source`.
    fn rearm(&mut self, source: MaskedSource) -> Result<(), Self::Error>;

    /// Returns a read-only snapshot of software-owned source state.
    ///
    /// Implementations must not read destructive device status, completion
    /// queues, or registers whose access advances hardware. The snapshot is
    /// intended for watchdog and recovery diagnostics in task context; it is
    /// never permission to poll for a missed completion.
    fn state(&self) -> Option<IrqSourceState> {
        None
    }
}

/// Software-owned mask state for one interrupt source.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IrqSourceMaskState {
    /// This endpoint owns and currently exposes the source to hardware.
    Armed,
    /// This endpoint owns and currently masks the source.
    Masked,
    /// A transport capability outside the portable driver owns masking.
    External,
}

/// Read-only task-context diagnostics for one interrupt source.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IrqSourceState {
    generation: NonZeroU64,
    delivery_enabled: bool,
    mask: IrqSourceMaskState,
    captures: u64,
    successful_rearms: u64,
    failed_rearms: u64,
}

impl IrqSourceState {
    /// Creates a snapshot from software-owned counters and state.
    pub const fn new(
        generation: NonZeroU64,
        delivery_enabled: bool,
        mask: IrqSourceMaskState,
        captures: u64,
        successful_rearms: u64,
        failed_rearms: u64,
    ) -> Self {
        Self {
            generation,
            delivery_enabled,
            mask,
            captures,
            successful_rearms,
            failed_rearms,
        }
    }

    pub const fn generation(self) -> NonZeroU64 {
        self.generation
    }

    pub const fn delivery_enabled(self) -> bool {
        self.delivery_enabled
    }

    pub const fn mask(self) -> IrqSourceMaskState {
        self.mask
    }

    pub const fn captures(self) -> u64 {
        self.captures
    }

    pub const fn successful_rearms(self) -> u64 {
        self.successful_rearms
    }

    pub const fn failed_rearms(self) -> u64 {
        self.failed_rearms
    }
}

/// Non-zero identity of a device source left masked by one capture pass.
///
/// The generation prevents a late worker from reopening a source after
/// recovery, shutdown, or a newer capture epoch. The bitmap lets one endpoint
/// represent a bounded set of independently maskable device causes without
/// leaking controller-specific register layouts into the OS runtime.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct MaskedSource {
    generation: NonZeroU64,
    bitmap: NonZeroU64,
}

impl MaskedSource {
    /// Creates a masked-source token from already-validated non-zero values.
    pub const fn new(generation: NonZeroU64, bitmap: NonZeroU64) -> Self {
        Self { generation, bitmap }
    }

    /// Validates raw generation and bitmap values at an FFI or register
    /// decoding boundary.
    ///
    /// # Errors
    ///
    /// Returns [`MaskedSourceError::ZeroGeneration`] when `generation` is
    /// zero, or [`MaskedSourceError::EmptyBitmap`] when `bitmap` is zero.
    pub const fn try_new(generation: u64, bitmap: u64) -> Result<Self, MaskedSourceError> {
        let Some(generation) = NonZeroU64::new(generation) else {
            return Err(MaskedSourceError::ZeroGeneration);
        };
        let Some(bitmap) = NonZeroU64::new(bitmap) else {
            return Err(MaskedSourceError::EmptyBitmap);
        };
        Ok(Self::new(generation, bitmap))
    }

    /// Returns the device activation epoch captured in this token.
    pub const fn generation(self) -> NonZeroU64 {
        self.generation
    }

    /// Returns the non-empty device-source bitmap held masked.
    pub const fn bitmap(self) -> NonZeroU64 {
        self.bitmap
    }
}

/// Invalid masked-source identity decoded at a public boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum MaskedSourceError {
    /// Zero is reserved for the absence of a source epoch.
    #[error("masked IRQ source generation must be nonzero")]
    ZeroGeneration,
    /// A token that owns no device source cannot authorize rearming anything.
    #[error("masked IRQ source bitmap must be nonempty")]
    EmptyBitmap,
}

/// Result of one bounded device interrupt capture.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use = "captured device facts or containment failures must be handled"]
pub enum IrqCapture<E, F> {
    /// This endpoint did not own the asserted shared interrupt.
    Unhandled,
    /// Required acknowledgement completed and `event` is now a stable fact.
    Captured {
        /// Immutable device facts safe to consume outside hard-IRQ context.
        event: E,
        /// Exact device source that remains masked until owner-side service
        /// completes. `None` means capture left the source armed.
        masked: Option<MaskedSource>,
    },
    /// Capture failed after classifying whether the device source is contained.
    Fault {
        /// Driver-specific failure reason.
        reason: F,
        /// Hardware containment already established by the endpoint.
        containment: FaultContainment,
    },
}

impl<E, F> IrqCapture<E, F> {
    /// Returns whether this endpoint acknowledged and captured stable facts.
    pub const fn is_captured(&self) -> bool {
        matches!(self, Self::Captured { .. })
    }

    /// Returns whether this endpoint did not own the shared interrupt.
    pub const fn is_unhandled(&self) -> bool {
        matches!(self, Self::Unhandled)
    }

    /// Returns whether capture failed after classifying containment.
    pub const fn is_fault(&self) -> bool {
        matches!(self, Self::Fault { .. })
    }

    /// Extracts the captured stable event, if present.
    pub fn captured(self) -> Option<(E, Option<MaskedSource>)> {
        match self {
            Self::Captured { event, masked } => Some((event, masked)),
            Self::Unhandled | Self::Fault { .. } => None,
        }
    }

    /// Extracts the device-specific fault and its containment state.
    pub fn fault(self) -> Option<(F, FaultContainment)> {
        match self {
            Self::Fault {
                reason,
                containment,
            } => Some((reason, containment)),
            Self::Unhandled | Self::Captured { .. } => None,
        }
    }

    /// Maps a stable event while preserving IRQ ownership and fault semantics.
    pub fn map_event<T>(self, map: impl FnOnce(E) -> T) -> IrqCapture<T, F> {
        match self {
            Self::Unhandled => IrqCapture::Unhandled,
            Self::Captured { event, masked } => IrqCapture::Captured {
                event: map(event),
                masked,
            },
            Self::Fault {
                reason,
                containment,
            } => IrqCapture::Fault {
                reason,
                containment,
            },
        }
    }
}

/// Whether a failed endpoint has stopped its exact device interrupt source.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FaultContainment {
    /// The endpoint masked the exact device source before reporting failure.
    DeviceSourceMasked(MaskedSource),
    /// Device interrupt generation may still be reaching the controller line.
    Uncontained,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Eq, PartialEq, thiserror::Error)]
    enum FakeRearmError {
        #[error("stale IRQ source generation: expected {expected}, got {actual}")]
        Stale { expected: u64, actual: u64 },
    }

    #[derive(Clone, Copy, Debug, thiserror::Error)]
    #[error("fake IRQ capture failed")]
    struct FakeCaptureFault;

    struct FakeEndpoint {
        containment_cause: Option<ContainmentCause>,
    }

    impl IrqEndpoint for FakeEndpoint {
        type Event = u8;
        type Fault = FakeCaptureFault;

        fn capture(&mut self) -> IrqCapture<Self::Event, Self::Fault> {
            IrqCapture::Captured {
                event: 3,
                masked: Some(MaskedSource::try_new(7, 0b101).unwrap()),
            }
        }

        fn contain(&mut self, cause: ContainmentCause) -> Result<MaskedSource, Self::Fault> {
            self.containment_cause = Some(cause);
            Ok(MaskedSource::try_new(8, 0b101).unwrap())
        }
    }

    struct FakeControl {
        generation: u64,
    }

    impl IrqSourceControl for FakeControl {
        type Error = FakeRearmError;

        fn rearm(&mut self, source: MaskedSource) -> Result<(), Self::Error> {
            let actual = source.generation().get();
            if actual != self.generation {
                return Err(FakeRearmError::Stale {
                    expected: self.generation,
                    actual,
                });
            }
            self.generation += 1;
            Ok(())
        }
    }

    #[test]
    fn masked_source_rejects_zero_generation_and_empty_bitmap() {
        assert_eq!(
            MaskedSource::try_new(0, 1),
            Err(MaskedSourceError::ZeroGeneration)
        );
        assert_eq!(
            MaskedSource::try_new(1, 0),
            Err(MaskedSourceError::EmptyBitmap)
        );
    }

    #[test]
    fn capture_publishes_mask_ownership_separately_from_the_event() {
        let mut endpoint = FakeEndpoint {
            containment_cause: None,
        };

        let IrqCapture::Captured { event, masked } = endpoint.capture() else {
            panic!("fake endpoint must capture one event")
        };
        assert_eq!(event, 3);
        let masked = masked.expect("the captured source remains device-masked");
        assert_eq!(masked.generation().get(), 7);
        assert_eq!(masked.bitmap().get(), 0b101);

        let contained = endpoint
            .contain(ContainmentCause::PublicationFull)
            .expect("the fake endpoint can mask its exact source");
        assert_eq!(contained.generation().get(), 8);
        assert_eq!(
            endpoint.containment_cause,
            Some(ContainmentCause::PublicationFull)
        );
    }

    #[test]
    fn owner_side_rearm_rejects_a_replayed_generation() {
        let source = MaskedSource::try_new(7, 1).unwrap();
        let mut control = FakeControl { generation: 7 };

        assert_eq!(control.rearm(source), Ok(()));
        assert_eq!(
            control.rearm(source),
            Err(FakeRearmError::Stale {
                expected: 8,
                actual: 7,
            })
        );
    }

    #[test]
    fn mapping_an_event_preserves_capture_semantics() {
        assert_eq!(
            IrqCapture::<u8, u16>::Captured {
                event: 3,
                masked: None,
            }
            .map_event(u16::from),
            IrqCapture::Captured {
                event: 3_u16,
                masked: None,
            }
        );
        assert_eq!(
            IrqCapture::<u8, u16>::Fault {
                reason: 7,
                containment: FaultContainment::DeviceSourceMasked(
                    MaskedSource::try_new(5, 1).unwrap(),
                ),
            }
            .map_event(u16::from),
            IrqCapture::Fault {
                reason: 7,
                containment: FaultContainment::DeviceSourceMasked(
                    MaskedSource::try_new(5, 1).unwrap(),
                ),
            }
        );
    }
}
