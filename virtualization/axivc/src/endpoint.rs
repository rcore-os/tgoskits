//! Channel endpoints encoding the single-producer, single-consumer contract.
//!
//! Every ring in an [`IvcRegion`] is SPSC. Message operations require `&mut
//! self`, and endpoint types are not `Clone`, so safe code cannot drive one
//! direction concurrently. Role attachment remains an `unsafe` boundary
//! because the caller must attach each shared region once per channel role.
//!
//! [`IvcRegion`]: crate::IvcRegion

use crate::{
    IVC_SLOT_SIZE,
    message::{IvcMessageReceiver, IvcMessageSender},
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

    /// Separates the full-duplex endpoints for independent tasks.
    pub fn into_parts(self) -> (IvcMessageSender<'a>, IvcMessageReceiver<'a>) {
        (self.sender, self.receiver)
    }
}

pub(crate) struct IvcSlotProducer<'a> {
    ring: &'a IvcRing,
}

impl<'a> IvcSlotProducer<'a> {
    pub(crate) const fn new(ring: &'a IvcRing) -> Self {
        Self { ring }
    }

    pub(crate) fn available_slots(&self) -> usize {
        self.ring.available_slots()
    }

    pub(crate) fn try_push_slot(&mut self, slot: &[u8; IVC_SLOT_SIZE]) -> Result<(), IvcSlotError> {
        self.ring.try_push_slot(slot)
    }
}

pub(crate) struct IvcSlotConsumer<'a> {
    ring: &'a IvcRing,
}

impl<'a> IvcSlotConsumer<'a> {
    pub(crate) const fn new(ring: &'a IvcRing) -> Self {
        Self { ring }
    }

    pub(crate) fn try_peek_slot(&mut self, output: &mut [u8; IVC_SLOT_SIZE]) -> bool {
        self.ring.try_peek_slot(output)
    }

    pub(crate) fn has_pending_slots(&self) -> bool {
        self.ring.has_pending_slots()
    }

    pub(crate) fn try_peek_slot_at(&self, offset: usize, output: &mut [u8; IVC_SLOT_SIZE]) -> bool {
        self.ring.try_peek_slot_at(offset, output)
    }

    pub(crate) fn pop_slot(&mut self) {
        self.ring.pop_slot();
    }
}
