//! Channel endpoints encoding the single-producer, single-consumer contract.
//!
//! Every ring in an [`IvcRegion`] is SPSC: one producer and one consumer may
//! drive it at a time. Endpoint operations require `&mut self`, and endpoint
//! types are not `Clone`, so safe code cannot drive one ring concurrently.
//! Role attachment remains an `unsafe` boundary because the caller must ensure
//! that each shared region is attached once for each channel role.
//!
//! [`IvcRegion`]: crate::IvcRegion

use crate::{
    message::{IvcMessage, IvcMessageKind},
    ring::{IvcRing, IvcRingError},
};

/// The producer and consumer owned by one side of an IVC channel.
///
/// Consume this value with [`Self::into_parts`] before moving the two endpoints
/// into independent sender and receiver tasks.
pub struct IvcEndpoints<'a> {
    producer: IvcProducer<'a>,
    consumer: IvcConsumer<'a>,
}

impl<'a> IvcEndpoints<'a> {
    pub(crate) const fn new(producer: &'a IvcRing, consumer: &'a IvcRing) -> Self {
        Self {
            producer: IvcProducer::new(producer),
            consumer: IvcConsumer::new(consumer),
        }
    }

    /// Separates the sending and receiving endpoints for independent tasks.
    pub fn into_parts(self) -> (IvcProducer<'a>, IvcConsumer<'a>) {
        (self.producer, self.consumer)
    }
}

/// Sending end of one SPSC ring.
///
/// Sending requires exclusive access and the endpoint is not cloneable. Safe
/// code therefore cannot share one producer between concurrent sender tasks.
///
/// ```compile_fail
/// use axivc::{IvcMessageKind, IvcProducer};
///
/// fn send_through_shared_reference(producer: &IvcProducer<'_>) {
///     let _ = producer.send(IvcMessageKind::Request, 1, b"message");
/// }
/// ```
///
/// ```compile_fail
/// use axivc::IvcProducer;
///
/// fn duplicate_producer(producer: IvcProducer<'_>) {
///     let _second = producer.clone();
/// }
/// ```
pub struct IvcProducer<'a> {
    ring: &'a IvcRing,
}

impl<'a> IvcProducer<'a> {
    const fn new(ring: &'a IvcRing) -> Self {
        Self { ring }
    }

    /// Appends one message to the ring.
    ///
    /// # Errors
    ///
    /// Returns [`IvcRingError::Full`] when the ring has no free slot, or
    /// [`IvcRingError::PayloadTooLarge`] when `payload` cannot fit in one
    /// fixed ring slot.
    pub fn send(
        &mut self,
        kind: IvcMessageKind,
        sequence: u64,
        payload: &[u8],
    ) -> Result<(), IvcRingError> {
        self.ring.send(kind, sequence, payload)
    }

    /// Returns whether at least one ring slot is currently available.
    pub fn can_send(&self) -> bool {
        self.ring.can_send()
    }
}

/// Receiving end of one SPSC ring.
///
/// Receiving requires exclusive access and the endpoint is not cloneable.
///
/// ```compile_fail
/// use axivc::IvcConsumer;
///
/// fn receive_through_shared_reference(consumer: &IvcConsumer<'_>) {
///     let mut payload = [0u8; 48];
///     let _ = consumer.try_recv(&mut payload);
/// }
/// ```
pub struct IvcConsumer<'a> {
    ring: &'a IvcRing,
}

impl<'a> IvcConsumer<'a> {
    const fn new(ring: &'a IvcRing) -> Self {
        Self { ring }
    }

    /// Copies out the oldest pending message, if any.
    ///
    /// # Errors
    ///
    /// Returns [`IvcRingError::BufferTooSmall`] without consuming the pending
    /// slot when `payload` is too short. Returns
    /// [`IvcRingError::UnknownMessageKind`] when the stored message kind is not
    /// known by this protocol version.
    pub fn try_recv(&mut self, payload: &mut [u8]) -> Result<Option<IvcMessage>, IvcRingError> {
        self.ring.try_recv(payload)
    }

    /// Returns whether a pending message is currently visible.
    pub fn can_recv(&self) -> bool {
        self.ring.can_recv()
    }
}
