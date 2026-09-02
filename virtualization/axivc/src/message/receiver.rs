use super::{
    IvcMessageError, IvcMessageMeta, IvcReceiveProgress,
    frame::{DecodedFrame, decode_frame},
};
use crate::{IVC_CELL_SIZE, endpoint::IvcCellConsumer};

/// Stateful nonblocking receiver for one direction of an IVC channel.
///
/// The receiver validates every frame before releasing its cell. It never
/// allocates from an untrusted declared message length and never splits one
/// cell fragment across caller buffers. Receiving requires exclusive access,
/// and this endpoint is not cloneable.
///
/// ```compile_fail
/// use axivc::IvcMessageReceiver;
///
/// fn read_through_shared_reference(receiver: &IvcMessageReceiver<'_>) {
///     let mut output = [0u8; 40];
///     let _ = receiver.try_read(&mut output);
/// }
/// ```
pub struct IvcMessageReceiver<'a> {
    consumer: IvcCellConsumer<'a>,
    state: ReceiveState,
}

impl<'a> IvcMessageReceiver<'a> {
    pub(crate) const fn new(consumer: IvcCellConsumer<'a>) -> Self {
        Self {
            consumer,
            state: ReceiveState::Idle,
        }
    }

    /// Returns metadata for the current or next message without consuming its
    /// first cell.
    ///
    /// Callers can use the untrusted declared length to enforce their own
    /// resource policy before reading or discarding the message.
    ///
    /// # Errors
    ///
    /// Returns a concrete protocol error if the next cell is not a valid first
    /// frame. Protocol errors poison this receiver because V1 has no reliable
    /// resynchronization marker.
    pub fn peek_message_meta(&mut self) -> Result<Option<IvcMessageMeta>, IvcMessageError> {
        match self.state {
            ReceiveState::Failed(error) => return Err(error),
            ReceiveState::Receiving(active) => return Ok(Some(active.meta)),
            ReceiveState::Idle => {}
        }

        let mut cell = [0u8; IVC_CELL_SIZE];
        if !self.consumer.try_peek_cell(&mut cell) {
            return Ok(None);
        }
        let frame = match decode_frame(&cell) {
            Ok(frame) => frame,
            Err(error) => return self.fail(error),
        };
        if frame.abort || !frame.first {
            return self.fail(IvcMessageError::MissingFirst);
        }
        Ok(Some(IvcMessageMeta::new(
            frame.message_id,
            frame.message_len,
        )))
    }

    /// Copies as many complete fragments as fit in `output`.
    ///
    /// If the next fragment does not fit and no earlier fragment was copied by
    /// this call, the method returns [`IvcMessageError::BufferTooSmall`] and
    /// leaves that cell at the ring head. If earlier fragments were copied, it
    /// returns their progress and leaves the next cell for a later call. An
    /// `output` of at least [`IVC_CELL_FRAGMENT_CAPACITY`] bytes always fits
    /// the next fragment, so such a buffer guarantees progress whenever a cell
    /// is available.
    ///
    /// [`IVC_CELL_FRAGMENT_CAPACITY`]: crate::IVC_CELL_FRAGMENT_CAPACITY
    ///
    /// # Errors
    ///
    /// Returns a concrete protocol error for malformed or inconsistent frames,
    /// or [`IvcMessageError::TransferAborted`] after consuming a valid peer
    /// `ABORT`. Protocol errors poison this receiver; buffer exhaustion does
    /// not.
    pub fn try_read(&mut self, output: &mut [u8]) -> Result<IvcReceiveProgress, IvcMessageError> {
        self.process_available_cells(Some(output))
    }

    /// Validates and discards available cells from the current or next message.
    ///
    /// This allows callers to reject an untrusted or over-limit message after
    /// inspecting [`Self::peek_message_meta`] without allocating its declared
    /// length.
    ///
    /// # Errors
    ///
    /// Returns the same protocol and abort errors as [`Self::try_read`].
    pub fn try_discard(&mut self) -> Result<IvcReceiveProgress, IvcMessageError> {
        self.process_available_cells(None)
    }

    fn process_available_cells(
        &mut self,
        mut output: Option<&mut [u8]>,
    ) -> Result<IvcReceiveProgress, IvcMessageError> {
        if let ReceiveState::Failed(error) = self.state {
            return Err(error);
        }

        let mut written = 0;
        let mut consumed_cells = 0;
        loop {
            let mut cell = [0u8; IVC_CELL_SIZE];
            if !self.consumer.try_peek_cell(&mut cell) {
                return Ok(IvcReceiveProgress::new(written, consumed_cells, false));
            }
            let frame = match decode_frame(&cell) {
                Ok(frame) => frame,
                Err(_) if consumed_cells > 0 => {
                    return Ok(IvcReceiveProgress::new(written, consumed_cells, false));
                }
                Err(error) => return self.fail(error),
            };
            let transition = match validate_transition(self.state, &frame) {
                Ok(transition) if transition.aborted && consumed_cells > 0 => {
                    return Ok(IvcReceiveProgress::new(written, consumed_cells, false));
                }
                Ok(transition) => transition,
                Err(_) if consumed_cells > 0 => {
                    return Ok(IvcReceiveProgress::new(written, consumed_cells, false));
                }
                Err(error) => return self.fail(error),
            };

            if let Some(target) = output.as_deref_mut() {
                let available = target.len() - written;
                if frame.fragment.len() > available {
                    if consumed_cells == 0 {
                        return Err(IvcMessageError::BufferTooSmall {
                            required: frame.fragment.len(),
                            provided: available,
                        });
                    }
                    return Ok(IvcReceiveProgress::new(written, consumed_cells, false));
                }
                target[written..written + frame.fragment.len()].copy_from_slice(frame.fragment);
                written += frame.fragment.len();
            }

            self.consumer.pop_cell();
            consumed_cells += 1;
            self.state = transition.next_state;
            if transition.aborted {
                return Err(IvcMessageError::TransferAborted);
            }
            if transition.complete {
                return Ok(IvcReceiveProgress::new(written, consumed_cells, true));
            }
        }
    }

    fn fail<T>(&mut self, error: IvcMessageError) -> Result<T, IvcMessageError> {
        self.state = ReceiveState::Failed(error);
        Err(error)
    }
}

fn validate_transition(
    state: ReceiveState,
    frame: &DecodedFrame<'_>,
) -> Result<ReceiveTransition, IvcMessageError> {
    let active = match state {
        ReceiveState::Idle => {
            if frame.abort || !frame.first {
                return Err(IvcMessageError::MissingFirst);
            }
            ActiveMessage {
                meta: IvcMessageMeta::new(frame.message_id, frame.message_len),
                received: 0,
            }
        }
        ReceiveState::Receiving(active) => {
            if frame.first {
                return Err(IvcMessageError::UnexpectedFirst);
            }
            ensure_frame_matches(active, frame)?;
            active
        }
        ReceiveState::Failed(error) => return Err(error),
    };

    if frame.abort {
        return Ok(ReceiveTransition {
            next_state: ReceiveState::Idle,
            complete: false,
            aborted: true,
        });
    }

    let received = active
        .received
        .checked_add(frame.fragment.len() as u64)
        .ok_or(IvcMessageError::MessageLengthExceeded {
            declared: active.meta.len(),
            received: u64::MAX,
        })?;
    if received > active.meta.len() {
        return Err(IvcMessageError::MessageLengthExceeded {
            declared: active.meta.len(),
            received,
        });
    }
    if frame.last && received != active.meta.len() {
        return Err(IvcMessageError::LengthMismatchAtLast {
            expected: active.meta.len(),
            actual: received,
        });
    }

    let complete = frame.last;
    let next_state = if complete {
        ReceiveState::Idle
    } else {
        ReceiveState::Receiving(ActiveMessage {
            meta: active.meta,
            received,
        })
    };
    Ok(ReceiveTransition {
        next_state,
        complete,
        aborted: false,
    })
}

fn ensure_frame_matches(
    active: ActiveMessage,
    frame: &DecodedFrame<'_>,
) -> Result<(), IvcMessageError> {
    if frame.message_id != active.meta.id() {
        return Err(IvcMessageError::UnexpectedMessageId {
            expected: active.meta.id(),
            actual: frame.message_id,
        });
    }
    if frame.message_len != active.meta.len() {
        return Err(IvcMessageError::InconsistentMessageLength {
            expected: active.meta.len(),
            actual: frame.message_len,
        });
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum ReceiveState {
    Idle,
    Receiving(ActiveMessage),
    Failed(IvcMessageError),
}

#[derive(Clone, Copy)]
struct ActiveMessage {
    meta: IvcMessageMeta,
    received: u64,
}

struct ReceiveTransition {
    next_state: ReceiveState,
    complete: bool,
    aborted: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        endpoint::{IvcCellConsumer, IvcCellProducer},
        message::{
            IvcMessageId,
            frame::{FrameSpec, encode_frame},
        },
        ring::{IvcRingDirection, new_ring_for_test},
    };

    #[test]
    fn receiver_rejects_message_id_changes_without_consuming_the_bad_cell() {
        let ring = new_ring_for_test();
        ring.initialize(IvcRingDirection::PublisherToSubscriber);
        let mut producer = IvcCellProducer::new(&ring);
        let mut receiver = IvcMessageReceiver::new(IvcCellConsumer::new(&ring));
        push_frame(&mut producer, 1, 80, true, false, &[0x11; 40]);

        let mut output = [0u8; 40];
        assert_eq!(receiver.try_read(&mut output).unwrap().written(), 40);
        push_frame(&mut producer, 2, 80, false, true, &[0x22; 40]);
        let error = IvcMessageError::UnexpectedMessageId {
            expected: IvcMessageId::new(1).unwrap(),
            actual: IvcMessageId::new(2).unwrap(),
        };
        assert_eq!(receiver.try_read(&mut output), Err(error));
        assert_eq!(receiver.try_discard(), Err(error));
    }

    #[test]
    fn receiver_rejects_inconsistent_length_and_short_last_frames() {
        let ring = new_ring_for_test();
        ring.initialize(IvcRingDirection::PublisherToSubscriber);
        let mut producer = IvcCellProducer::new(&ring);
        let mut receiver = IvcMessageReceiver::new(IvcCellConsumer::new(&ring));
        push_frame(&mut producer, 1, 80, true, false, &[0x11; 40]);

        let mut output = [0u8; 40];
        receiver.try_read(&mut output).unwrap();
        push_frame(&mut producer, 1, 79, false, true, &[0x22; 39]);
        assert_eq!(
            receiver.try_read(&mut output),
            Err(IvcMessageError::InconsistentMessageLength {
                expected: 80,
                actual: 79,
            })
        );

        let second_ring = new_ring_for_test();
        second_ring.initialize(IvcRingDirection::PublisherToSubscriber);
        let mut producer = IvcCellProducer::new(&second_ring);
        let mut receiver = IvcMessageReceiver::new(IvcCellConsumer::new(&second_ring));
        push_frame(&mut producer, 1, 80, true, false, &[0x11; 40]);
        receiver.try_read(&mut output).unwrap();
        push_frame(&mut producer, 1, 80, false, true, &[0x22; 39]);
        assert_eq!(
            receiver.try_read(&mut output),
            Err(IvcMessageError::LengthMismatchAtLast {
                expected: 80,
                actual: 79,
            })
        );
    }

    #[test]
    fn receiver_rejects_fragments_beyond_the_declared_length() {
        let ring = new_ring_for_test();
        ring.initialize(IvcRingDirection::PublisherToSubscriber);
        let mut producer = IvcCellProducer::new(&ring);
        let mut receiver = IvcMessageReceiver::new(IvcCellConsumer::new(&ring));
        push_frame(&mut producer, 1, 40, true, false, &[0x11; 40]);

        let mut output = [0u8; 40];
        receiver.try_read(&mut output).unwrap();
        push_frame(&mut producer, 1, 40, false, true, &[0x22]);
        assert_eq!(
            receiver.try_read(&mut output),
            Err(IvcMessageError::MessageLengthExceeded {
                declared: 40,
                received: 41,
            })
        );
    }

    fn push_frame(
        producer: &mut IvcCellProducer<'_>,
        message_id: u64,
        message_len: u64,
        first: bool,
        last: bool,
        fragment: &[u8],
    ) {
        let mut cell = [0u8; IVC_CELL_SIZE];
        encode_frame(
            &mut cell,
            FrameSpec {
                message_id: IvcMessageId::new(message_id).unwrap(),
                message_len,
                first,
                last,
                abort: false,
            },
            fragment,
        )
        .unwrap();
        producer.try_push_cell(&cell).unwrap();
    }
}
