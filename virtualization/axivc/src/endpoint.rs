//! Channel endpoints encoding the single-producer, single-consumer contract.
//!
//! Every direction is SPSC. Message operations require `&mut self`, and
//! endpoint types are not `Clone`, so safe code cannot drive one direction
//! concurrently. Role attachment remains an `unsafe` boundary because the
//! caller must attach each shared region once per channel role.
//!
//! [`IvcRegion`]: crate::IvcRegion

use crate::{
    IVC_SLOT_SIZE,
    ivshmem::{IvshmemLocalChannel, IvshmemPeerChannel, IvshmemSlotConsumer, IvshmemSlotProducer},
    message::{IvcMessageError, IvcMessageReceiver, IvcMessageSender},
    ring::{IvcRing, IvcSlotError},
};

/// The message sender and receiver owned by one side of an IVC channel.
///
/// Consume this value with [`Self::into_parts`] before moving the two endpoints
/// into independent sender and receiver tasks.
pub struct IvcEndpoints<'a> {
    sender: IvcMessageSender<'a>,
    receiver: IvcMessageReceiver<'a>,
}

impl<'a> IvcEndpoints<'a> {
    pub(crate) const fn new(producer: &'a IvcRing, consumer: &'a IvcRing) -> Self {
        Self {
            sender: IvcMessageSender::new(IvcSlotProducer::new(producer)),
            receiver: IvcMessageReceiver::new(IvcSlotConsumer::new(consumer)),
        }
    }

    pub(crate) const fn new_ivshmem(
        local: IvshmemLocalChannel<'a>,
        peer: IvshmemPeerChannel<'a>,
        local_incarnation: u32,
        peer_incarnation: u32,
    ) -> Self {
        Self {
            sender: IvcMessageSender::new(IvcSlotProducer::new_ivshmem(
                local,
                peer,
                local_incarnation,
                peer_incarnation,
            )),
            receiver: IvcMessageReceiver::new(IvcSlotConsumer::new_ivshmem(
                local,
                peer,
                local_incarnation,
                peer_incarnation,
            )),
        }
    }

    /// Separates the full-duplex endpoints for independent tasks.
    pub fn into_parts(self) -> (IvcMessageSender<'a>, IvcMessageReceiver<'a>) {
        (self.sender, self.receiver)
    }
}

pub(crate) struct IvcSlotProducer<'a> {
    backend: SlotProducerBackend<'a>,
}

impl<'a> IvcSlotProducer<'a> {
    pub(crate) const fn new(ring: &'a IvcRing) -> Self {
        Self {
            backend: SlotProducerBackend::Ring(ring),
        }
    }

    pub(crate) const fn new_ivshmem(
        local: IvshmemLocalChannel<'a>,
        peer: IvshmemPeerChannel<'a>,
        local_incarnation: u32,
        peer_incarnation: u32,
    ) -> Self {
        Self {
            backend: SlotProducerBackend::Ivshmem(IvshmemSlotProducer::new(
                local,
                peer,
                local_incarnation,
                peer_incarnation,
            )),
        }
    }

    pub(crate) fn available_slots(&self) -> usize {
        match &self.backend {
            SlotProducerBackend::Ring(ring) => ring.available_slots(),
            SlotProducerBackend::Ivshmem(producer) => producer.available_slots(),
        }
    }

    pub(crate) fn try_push_slot(&mut self, slot: &[u8; IVC_SLOT_SIZE]) -> Result<(), IvcSlotError> {
        match &mut self.backend {
            SlotProducerBackend::Ring(ring) => ring.try_push_slot(slot),
            SlotProducerBackend::Ivshmem(producer) => producer.try_push_slot(slot),
        }
    }
}

pub(crate) struct IvcSlotConsumer<'a> {
    backend: SlotConsumerBackend<'a>,
}

impl<'a> IvcSlotConsumer<'a> {
    pub(crate) const fn new(ring: &'a IvcRing) -> Self {
        Self {
            backend: SlotConsumerBackend::Ring(ring),
        }
    }

    pub(crate) const fn new_ivshmem(
        local: IvshmemLocalChannel<'a>,
        peer: IvshmemPeerChannel<'a>,
        local_incarnation: u32,
        peer_incarnation: u32,
    ) -> Self {
        Self {
            backend: SlotConsumerBackend::Ivshmem(IvshmemSlotConsumer::new(
                local,
                peer,
                local_incarnation,
                peer_incarnation,
            )),
        }
    }

    pub(crate) fn try_peek_slot(
        &mut self,
        output: &mut [u8; IVC_SLOT_SIZE],
    ) -> Result<bool, IvcSlotError> {
        match &mut self.backend {
            SlotConsumerBackend::Ring(ring) => Ok(ring.try_peek_slot(output)),
            SlotConsumerBackend::Ivshmem(consumer) => consumer.try_peek_slot(output),
        }
    }

    pub(crate) fn has_pending_slots(&self) -> bool {
        match &self.backend {
            SlotConsumerBackend::Ring(ring) => ring.has_pending_slots(),
            SlotConsumerBackend::Ivshmem(consumer) => consumer.has_pending_slots(),
        }
    }

    pub(crate) fn try_peek_slot_at(
        &self,
        offset: usize,
        output: &mut [u8; IVC_SLOT_SIZE],
    ) -> Result<bool, IvcSlotError> {
        match &self.backend {
            SlotConsumerBackend::Ring(ring) => Ok(ring.try_peek_slot_at(offset, output)),
            SlotConsumerBackend::Ivshmem(consumer) => consumer.try_peek_slot_at(offset, output),
        }
    }

    pub(crate) fn pop_slot(&mut self) -> Result<(), IvcSlotError> {
        match &mut self.backend {
            SlotConsumerBackend::Ring(ring) => {
                ring.pop_slot();
                Ok(())
            }
            SlotConsumerBackend::Ivshmem(consumer) => consumer.pop_slot(),
        }
    }
}

enum SlotProducerBackend<'a> {
    Ring(&'a IvcRing),
    Ivshmem(IvshmemSlotProducer<'a>),
}

enum SlotConsumerBackend<'a> {
    Ring(&'a IvcRing),
    Ivshmem(IvshmemSlotConsumer<'a>),
}

pub(crate) const fn slot_error(error: IvcSlotError) -> IvcMessageError {
    match error {
        IvcSlotError::Full => IvcMessageError::SlotFull,
        IvcSlotError::SessionLost => IvcMessageError::SessionLost,
        IvcSlotError::CursorOutOfRange { producer, consumer } => {
            IvcMessageError::CursorOutOfRange { producer, consumer }
        }
    }
}
