use super::{
    IvcMessageError, IvcMessageMeta, IvcReceiveProgress,
    frame::{DecodedFrame, decode_frame},
};
use crate::{IVC_RING_CAPACITY, IVC_SLOT_SIZE, endpoint::IvcSlotConsumer};

/// Stateful nonblocking receiver for one direction of an IVC channel.
///
/// The receiver validates every frame before releasing its slot. It never
/// allocates from an untrusted declared message length and never splits one
/// slot fragment across caller buffers. Receiving requires exclusive access,
/// and this endpoint is not cloneable.
///
/// ```compile_fail
/// use axivc::IvcMessageReceiver;
///
/// fn read_through_shared_reference(receiver: &IvcMessageReceiver<'_>) {
///     let mut output = [0u8; axivc::IVC_SLOT_FRAGMENT_CAPACITY];
///     let _ = receiver.try_read(&mut output);
/// }
/// ```
pub struct IvcMessageReceiver<'a> {
    consumer: IvcSlotConsumer<'a>,
    state: ReceiveState,
}

impl<'a> IvcMessageReceiver<'a> {
    pub(crate) const fn new(consumer: IvcSlotConsumer<'a>) -> Self {
        Self {
            consumer,
            state: ReceiveState::Idle,
        }
    }

    /// Returns whether a read can inspect a queued frame or report a retained
    /// protocol error. An active message with no queued continuation is not
    /// ready. This does not promise that a complete message is available.
    pub fn can_receive(&self) -> bool {
        matches!(self.state, ReceiveState::Failed(_)) || self.consumer.has_pending_slots()
    }

    /// Checks whether the current or next message can finish without waiting.
    ///
    /// Inspects at most one ring of frames without consuming slots or changing
    /// receiver state. A valid ABORT counts as a terminal event. This supports
    /// all-or-nothing nonblocking device reads; messages larger than the ring
    /// must instead use streaming reads with backpressure.
    ///
    /// # Errors
    ///
    /// Reports [`IvcMessageError::StreamingRequired`] if a full ring contains
    /// no terminal frame, even when the declared length fits in one ring of
    /// maximally sized fragments. `Ok(false)` means more frames can still arrive
    /// without consuming slots. Also reports invalid frames and retained
    /// protocol errors. This inspection alone does not poison the receiver.
    pub fn complete_message_available(&self) -> Result<bool, IvcMessageError> {
        let mut state = self.state;
        if let ReceiveState::Failed(error) = state {
            return Err(error);
        }
        let mut slot = [0; IVC_SLOT_SIZE];
        for offset in 0..IVC_RING_CAPACITY {
            if !self.consumer.try_peek_slot_at(offset, &mut slot) {
                return Ok(false);
            }
            let frame = decode_frame(&slot)?;
            let transition = validate_transition(state, &frame)?;
            if transition.complete || transition.aborted {
                return Ok(true);
            }
            state = transition.next_state;
        }
        Err(IvcMessageError::StreamingRequired)
    }

    /// Returns metadata for the current or next message without consuming its
    /// first slot.
    ///
    /// Callers can use the untrusted declared length to enforce their own
    /// resource policy before reading or discarding the message.
    ///
    /// # Errors
    ///
    /// Returns a concrete protocol error if the next slot is not a valid first
    /// frame. Protocol errors poison this receiver because V1 has no reliable
    /// resynchronization marker.
    pub fn peek_message_meta(&mut self) -> Result<Option<IvcMessageMeta>, IvcMessageError> {
        match self.state {
            ReceiveState::Failed(error) => return Err(error),
            ReceiveState::Receiving(active) => return Ok(Some(active.meta)),
            ReceiveState::Idle => {}
        }

        let mut slot = [0u8; IVC_SLOT_SIZE];
        if !self.consumer.try_peek_slot(&mut slot) {
            return Ok(None);
        }
        let frame = match decode_frame(&slot) {
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
    /// leaves that slot at the ring head. If earlier fragments were copied, it
    /// returns their progress and leaves the next slot for a later call. An
    /// `output` of at least [`IVC_SLOT_FRAGMENT_CAPACITY`] bytes always fits
    /// the next fragment, so such a buffer guarantees progress whenever a slot
    /// is available.
    ///
    /// [`IVC_SLOT_FRAGMENT_CAPACITY`]: crate::IVC_SLOT_FRAGMENT_CAPACITY
    ///
    /// # Errors
    ///
    /// Returns a concrete protocol error for malformed or inconsistent frames,
    /// or [`IvcMessageError::TransferAborted`] after consuming a valid peer
    /// `ABORT`. Protocol errors poison this receiver; buffer exhaustion does
    /// not.
    pub fn try_read(&mut self, output: &mut [u8]) -> Result<IvcReceiveProgress, IvcMessageError> {
        self.process_available_slots(Some(output))
    }

    /// Validates and discards available slots from the current or next message.
    ///
    /// This allows callers to reject an untrusted or over-limit message after
    /// inspecting [`Self::peek_message_meta`] without allocating its declared
    /// length.
    ///
    /// # Errors
    ///
    /// Returns the same protocol and abort errors as [`Self::try_read`].
    pub fn try_discard(&mut self) -> Result<IvcReceiveProgress, IvcMessageError> {
        self.process_available_slots(None)
    }

    fn process_available_slots(
        &mut self,
        mut output: Option<&mut [u8]>,
    ) -> Result<IvcReceiveProgress, IvcMessageError> {
        if let ReceiveState::Failed(error) = self.state {
            return Err(error);
        }

        let mut written = 0;
        let mut consumed_slots = 0;
        loop {
            let mut slot = [0u8; IVC_SLOT_SIZE];
            if !self.consumer.try_peek_slot(&mut slot) {
                return Ok(IvcReceiveProgress::new(written, consumed_slots, false));
            }
            let frame = match decode_frame(&slot) {
                Ok(frame) => frame,
                Err(_) if consumed_slots > 0 => {
                    return Ok(IvcReceiveProgress::new(written, consumed_slots, false));
                }
                Err(error) => return self.fail(error),
            };
            let transition = match validate_transition(self.state, &frame) {
                Ok(transition) if transition.aborted && consumed_slots > 0 => {
                    return Ok(IvcReceiveProgress::new(written, consumed_slots, false));
                }
                Ok(transition) => transition,
                Err(_) if consumed_slots > 0 => {
                    return Ok(IvcReceiveProgress::new(written, consumed_slots, false));
                }
                Err(error) => return self.fail(error),
            };

            if let Some(target) = output.as_deref_mut() {
                let available = target.len() - written;
                if frame.fragment.len() > available {
                    if consumed_slots == 0 {
                        return Err(IvcMessageError::BufferTooSmall {
                            required: frame.fragment.len(),
                            provided: available,
                        });
                    }
                    return Ok(IvcReceiveProgress::new(written, consumed_slots, false));
                }
                target[written..written + frame.fragment.len()].copy_from_slice(frame.fragment);
                written += frame.fragment.len();
            }

            self.consumer.pop_slot();
            consumed_slots += 1;
            self.state = transition.next_state;
            if transition.aborted {
                return Err(IvcMessageError::TransferAborted);
            }
            if transition.complete {
                return Ok(IvcReceiveProgress::new(written, consumed_slots, true));
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
        endpoint::{IvcSlotConsumer, IvcSlotProducer},
        message::{
            IvcMessageId,
            frame::{FrameSpec, encode_frame},
        },
        ring::{IvcRingDirection, new_ring_for_test},
    };

    #[test]
    fn receiver_rejects_message_id_changes_without_consuming_the_bad_slot() {
        let ring = new_ring_for_test();
        ring.initialize(IvcRingDirection::PublisherToSubscriber);
        let mut producer = IvcSlotProducer::new(&ring);
        let mut receiver = IvcMessageReceiver::new(IvcSlotConsumer::new(&ring));
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
        let mut producer = IvcSlotProducer::new(&ring);
        let mut receiver = IvcMessageReceiver::new(IvcSlotConsumer::new(&ring));
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
        let mut producer = IvcSlotProducer::new(&second_ring);
        let mut receiver = IvcMessageReceiver::new(IvcSlotConsumer::new(&second_ring));
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
        let mut producer = IvcSlotProducer::new(&ring);
        let mut receiver = IvcMessageReceiver::new(IvcSlotConsumer::new(&ring));
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
        producer: &mut IvcSlotProducer<'_>,
        message_id: u64,
        message_len: u64,
        first: bool,
        last: bool,
        fragment: &[u8],
    ) {
        let mut slot = [0u8; IVC_SLOT_SIZE];
        encode_frame(
            &mut slot,
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
        producer.try_push_slot(&slot).unwrap();
    }
}
