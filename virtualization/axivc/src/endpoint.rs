//! Channel endpoints encoding the single-producer, single-consumer contract.
//!
//! Every ring in an [`IvcRegion`] is SPSC. Message operations require `&mut
//! self`, and endpoint types are not `Clone`, so safe code cannot drive one
//! direction concurrently. Role attachment remains an `unsafe` boundary
//! because the caller must attach each shared region once per channel role.
//!
//! [`IvcRegion`]: crate::IvcRegion

use crate::{
    IVC_CELL_SIZE,
    message::{IvcMessageReceiver, IvcMessageSender},
    ring::{IvcCellError, IvcRing},
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
            sender: IvcMessageSender::new(IvcCellProducer::new(producer)),
            receiver: IvcMessageReceiver::new(IvcCellConsumer::new(consumer)),
        }
    }

    /// Separates the full-duplex endpoints for independent tasks.
    pub fn into_parts(self) -> (IvcMessageSender<'a>, IvcMessageReceiver<'a>) {
        (self.sender, self.receiver)
    }
}

pub(crate) struct IvcCellProducer<'a> {
    ring: &'a IvcRing,
}

impl<'a> IvcCellProducer<'a> {
    pub(crate) const fn new(ring: &'a IvcRing) -> Self {
        Self { ring }
    }

    pub(crate) fn try_push_cell(&mut self, cell: &[u8; IVC_CELL_SIZE]) -> Result<(), IvcCellError> {
        self.ring.try_push_cell(cell)
    }
}

pub(crate) struct IvcCellConsumer<'a> {
    ring: &'a IvcRing,
}

impl<'a> IvcCellConsumer<'a> {
    pub(crate) const fn new(ring: &'a IvcRing) -> Self {
        Self { ring }
    }

    pub(crate) fn try_peek_cell(&mut self, output: &mut [u8; IVC_CELL_SIZE]) -> bool {
        self.ring.try_peek_cell(output)
    }

    pub(crate) fn pop_cell(&mut self) {
        self.ring.pop_cell();
    }
}
