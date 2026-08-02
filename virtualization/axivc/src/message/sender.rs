use core::cmp;

use super::{
    IvcMessageError, IvcMessageId, IvcMessageMeta, IvcSendProgress,
    frame::{FrameSpec, encode_frame},
};
use crate::{IVC_CELL_FRAGMENT_CAPACITY, IVC_CELL_SIZE, endpoint::IvcCellProducer};

/// Stateful nonblocking sender for one direction of an IVC channel.
///
/// A sender preserves frame ordering by allowing only one active logical
/// message. Repeated [`Self::try_write`] calls can therefore send messages much
/// larger than the cell ring without allocation. Sending requires exclusive
/// access, and this endpoint is not cloneable.
///
/// ```compile_fail
/// use axivc::IvcMessageSender;
///
/// fn start_through_shared_reference(sender: &IvcMessageSender<'_>) {
///     let _ = sender.start_message(1);
/// }
/// ```
///
/// ```compile_fail
/// use axivc::IvcMessageSender;
///
/// fn duplicate_sender(sender: IvcMessageSender<'_>) {
///     let _second = sender.clone();
/// }
/// ```
pub struct IvcMessageSender<'a> {
    producer: IvcCellProducer<'a>,
    state: SendState,
    next_message_id: Option<IvcMessageId>,
}

impl<'a> IvcMessageSender<'a> {
    pub(crate) const fn new(producer: IvcCellProducer<'a>) -> Self {
        Self {
            producer,
            state: SendState::Idle,
            next_message_id: IvcMessageId::new(1),
        }
    }

    /// Starts one logical message and returns its transport identifier.
    ///
    /// This changes only local state. The first cell is published by
    /// [`Self::try_write`]. Application request identifiers must be encoded in
    /// the payload rather than derived from this transport identifier.
    ///
    /// # Errors
    ///
    /// Returns [`IvcMessageError::SendInProgress`] if a previous message has
    /// not published `LAST` or `ABORT`. Returns
    /// [`IvcMessageError::MessageIdExhausted`] after all V1 identifiers have
    /// been assigned.
    pub fn start_message(&mut self, message_len: u64) -> Result<IvcMessageId, IvcMessageError> {
        if !matches!(self.state, SendState::Idle) {
            return Err(IvcMessageError::SendInProgress);
        }

        let message_id = self
            .next_message_id
            .ok_or(IvcMessageError::MessageIdExhausted)?;
        self.next_message_id = message_id.get().checked_add(1).and_then(IvcMessageId::new);
        self.state = SendState::Sending(SendingMessage {
            meta: IvcMessageMeta::new(message_id, message_len),
            sent: 0,
            published_any: false,
        });
        Ok(message_id)
    }

    /// Publishes as many complete fragment cells as current ring space allows.
    ///
    /// The returned consumed count identifies the prefix of `input` that the
    /// caller may release. Ring-full backpressure is reported as successful
    /// progress with `complete == false`; no cell is overwritten.
    ///
    /// Empty messages are published by calling this method with `input == []`
    /// after `start_message(0)`.
    ///
    /// # Errors
    ///
    /// Returns [`IvcMessageError::NoMessageInProgress`] unless
    /// [`Self::start_message`] has started a message. Returns
    /// [`IvcMessageError::InputExceedsRemaining`] without publishing input if
    /// the supplied slice is longer than the declared unsent payload.
    pub fn try_write(&mut self, input: &[u8]) -> Result<IvcSendProgress, IvcMessageError> {
        let SendState::Sending(mut sending) = self.state else {
            return Err(IvcMessageError::NoMessageInProgress);
        };
        let remaining = sending.meta.len() - sending.sent;
        if input.len() as u128 > remaining as u128 {
            return Err(IvcMessageError::InputExceedsRemaining {
                remaining,
                provided: input.len(),
            });
        }

        if sending.meta.is_empty() {
            return self.publish_empty_message(sending);
        }

        let mut consumed = 0;
        let mut published_cells = 0;
        while consumed < input.len() {
            let fragment_len = cmp::min(IVC_CELL_FRAGMENT_CAPACITY, input.len() - consumed);
            // The `input.len() <= remaining` check above keeps `sent` below
            // the declared message length, so this addition cannot overflow.
            let next_sent = sending.sent + fragment_len as u64;
            let complete = next_sent == sending.meta.len();
            let fragment = &input[consumed..consumed + fragment_len];
            let cell = encode_message_cell(sending, complete, fragment)?;
            if self.producer.try_push_cell(&cell).is_err() {
                break;
            }

            sending.sent = next_sent;
            sending.published_any = true;
            consumed += fragment_len;
            published_cells += 1;
            if complete {
                self.state = SendState::Idle;
                return Ok(IvcSendProgress::new(consumed, published_cells, true));
            }
        }

        self.state = SendState::Sending(sending);
        Ok(IvcSendProgress::new(consumed, published_cells, false))
    }

    /// Aborts the active logical message.
    ///
    /// If no frame has been published, cancellation is local and consumes no
    /// ring space. Otherwise an `ABORT` cell is published so the receiver can
    /// discard its partial message.
    ///
    /// # Errors
    ///
    /// Returns [`IvcMessageError::NoMessageInProgress`] if the sender is idle,
    /// or [`IvcMessageError::CellFull`] if an `ABORT` cell is required but the
    /// ring is full. A full ring leaves the send active so the caller can retry.
    pub fn try_abort(&mut self) -> Result<(), IvcMessageError> {
        let SendState::Sending(sending) = self.state else {
            return Err(IvcMessageError::NoMessageInProgress);
        };
        if !sending.published_any {
            self.state = SendState::Idle;
            return Ok(());
        }

        let mut cell = [0u8; IVC_CELL_SIZE];
        encode_frame(
            &mut cell,
            FrameSpec {
                message_id: sending.meta.id(),
                message_len: sending.meta.len(),
                first: false,
                last: false,
                abort: true,
            },
            &[],
        )?;
        self.producer
            .try_push_cell(&cell)
            .map_err(|_| IvcMessageError::CellFull)?;
        self.state = SendState::Idle;
        Ok(())
    }

    fn publish_empty_message(
        &mut self,
        sending: SendingMessage,
    ) -> Result<IvcSendProgress, IvcMessageError> {
        let cell = encode_message_cell(sending, true, &[])?;
        if self.producer.try_push_cell(&cell).is_err() {
            return Ok(IvcSendProgress::new(0, 0, false));
        }
        self.state = SendState::Idle;
        Ok(IvcSendProgress::new(0, 1, true))
    }
}

fn encode_message_cell(
    sending: SendingMessage,
    complete: bool,
    fragment: &[u8],
) -> Result<[u8; IVC_CELL_SIZE], IvcMessageError> {
    let mut cell = [0u8; IVC_CELL_SIZE];
    encode_frame(
        &mut cell,
        FrameSpec {
            message_id: sending.meta.id(),
            message_len: sending.meta.len(),
            first: !sending.published_any,
            last: complete,
            abort: false,
        },
        fragment,
    )?;
    Ok(cell)
}

#[derive(Clone, Copy)]
enum SendState {
    Idle,
    Sending(SendingMessage),
}

#[derive(Clone, Copy)]
struct SendingMessage {
    meta: IvcMessageMeta,
    sent: u64,
    published_any: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        IVC_RING_CAPACITY,
        ring::{IvcRingDirection, new_ring_for_test},
    };

    #[test]
    fn sender_rejects_interleaving_and_exhausts_ids_without_wrapping() {
        let ring = new_ring_for_test();
        ring.initialize(IvcRingDirection::PublisherToSubscriber);
        let mut sender = IvcMessageSender::new(IvcCellProducer::new(&ring));

        sender.next_message_id = IvcMessageId::new(u64::MAX);
        let last_id = sender.start_message(1).unwrap();
        assert_eq!(last_id.get(), u64::MAX);
        assert_eq!(
            sender.start_message(1),
            Err(IvcMessageError::SendInProgress)
        );
        sender.try_abort().unwrap();
        assert_eq!(
            sender.start_message(1),
            Err(IvcMessageError::MessageIdExhausted)
        );
    }

    #[test]
    fn abort_preserves_send_state_when_the_ring_is_full() {
        let ring = new_ring_for_test();
        ring.initialize(IvcRingDirection::PublisherToSubscriber);
        let mut sender = IvcMessageSender::new(IvcCellProducer::new(&ring));
        let payload = [0x5a; IVC_RING_CAPACITY * IVC_CELL_FRAGMENT_CAPACITY];

        sender.start_message((payload.len() + 1) as u64).unwrap();
        let progress = sender.try_write(&payload).unwrap();
        assert_eq!(progress.published_cells(), IVC_RING_CAPACITY);
        assert!(!progress.is_complete());
        assert_eq!(sender.try_abort(), Err(IvcMessageError::CellFull));
        assert_eq!(
            sender.start_message(1),
            Err(IvcMessageError::SendInProgress)
        );
    }
}
