//! Doorbell value objects and the event sink boundary between link and peer.
//!
//! A BAR0 doorbell write is decoded into a [`Doorbell`], routed by the link,
//! and handed to the target peer's [`IvshmemEventSink`] as a
//! [`DoorbellEvent`]. These types are pure values: they hold no locks and
//! perform no allocation, so the link and endpoint adapters can share them in
//! `no_std + alloc` builds and tests can exercise routing without a PCI
//! runtime.

use super::{error::IvshmemError, link::PeerId};

/// One decoded BAR0 doorbell write.
///
/// Pure value with no validation state: every 32-bit write decodes to a
/// syntactically valid target/vector pair. Whether the target exists in the
/// link profile and whether the vector is supported are routing-time checks
/// that live where the link profile lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Doorbell {
    target: PeerId,
    vector: u16,
}

impl Doorbell {
    /// Splits `value` into target (upper 16 bits) and vector (lower 16 bits).
    ///
    /// Infallible by design: the QEMU doorbell encoding partitions every
    /// `u32` into a valid pair, so decode failures cannot exist and the error
    /// type needs no decode variant.
    pub const fn from_write(value: u32) -> Self {
        Self {
            target: PeerId::new((value >> 16) as u16),
            vector: value as u16,
        }
    }

    /// Returns the addressed peer (unchecked at decode time).
    pub const fn target(self) -> PeerId {
        self.target
    }

    /// Returns the addressed vector; the current profile only supports 0.
    pub const fn vector(self) -> u16 {
        self.vector
    }
}

/// One routed doorbell, handed to the target peer's sink outside all locks.
///
/// The source identity comes from the writing endpoint's attachment, never
/// from the written value, so a guest cannot forge the sender.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DoorbellEvent {
    source: PeerId,
    target: PeerId,
    vector: u16,
}

impl DoorbellEvent {
    /// Assembles the event the link delivers to `doorbell.target()`'s sink.
    pub const fn new(source: PeerId, doorbell: Doorbell) -> Self {
        Self {
            source,
            target: doorbell.target(),
            vector: doorbell.vector(),
        }
    }

    /// Returns the sending peer.
    pub const fn source(self) -> PeerId {
        self.source
    }

    /// Returns the receiving peer.
    pub const fn target(self) -> PeerId {
        self.target
    }

    /// Returns the addressed vector.
    pub const fn vector(self) -> u16 {
        self.vector
    }
}

/// One peer's ability to receive link events.
///
/// F3's only implementation records the event in the target endpoint's
/// registers; the later MSI-X feature adds a message-transport
/// implementation behind the same boundary. Implementations must not call
/// back into the link and must not take the link peer-table lock: the link
/// invokes them after releasing that lock.
pub trait IvshmemEventSink: Send + Sync {
    /// Records one doorbell event on the target endpoint.
    ///
    /// # Errors
    ///
    /// Returns [`IvshmemError::EventDeliveryFailed`] when the target endpoint
    /// cannot record the event (for example a poisoned register lock). The
    /// link treats every error as a rate-limited diagnostic; a guest doorbell
    /// never becomes a device access error or a VM abort.
    fn deliver(&self, event: DoorbellEvent) -> Result<(), IvshmemError>;
}

#[cfg(test)]
mod tests {
    use alloc::format;

    use super::*;

    const fn doorbell(target: u16, vector: u16) -> Doorbell {
        Doorbell::from_write(((target as u32) << 16) | vector as u32)
    }

    #[test]
    fn decodes_target_and_vector_from_the_written_dword() {
        let decoded = doorbell(1, 2);
        assert_eq!(decoded.target(), PeerId::new(1));
        assert_eq!(decoded.vector(), 2);
        assert_eq!(Doorbell::from_write(0), doorbell(0, 0));
        // Every u32 decodes: the upper and lower halves partition cleanly.
        let saturated = Doorbell::from_write(0xffff_ffff);
        assert_eq!(saturated.target(), PeerId::new(0xffff));
        assert_eq!(saturated.vector(), 0xffff);
    }

    #[test]
    fn events_take_the_source_from_the_link_not_the_write() {
        let event = DoorbellEvent::new(PeerId::new(0), doorbell(1, 0));
        assert_eq!(event.source(), PeerId::new(0));
        assert_eq!(event.target(), PeerId::new(1));
        assert_eq!(event.vector(), 0);
    }

    #[test]
    fn doorbell_values_render_for_diagnostics() {
        assert_eq!(
            format!("{:?}", doorbell(1, 0)),
            "Doorbell { target: PeerId(1), vector: 0 }"
        );
    }
}
