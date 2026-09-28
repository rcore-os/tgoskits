//! Message V1 framing over peer-addressed ivshmem slots.
//!
//! The 24-byte frame format follows the existing axivc Message V1 wire
//! contract. This module is independent of the axivc crate: a sender pins
//! one destination until LAST or ABORT, and each publisher has separate
//! receive state so other publishers may progress independently.

use crate::{Error, MAX_PEERS, PAYLOAD_SIZE, Peer};

/// Bytes in the Message V1 header carried within a data slot.
pub const HEADER_SIZE: usize = 24;
/// Maximum application bytes in one Message V1 frame.
pub const FRAGMENT_SIZE: usize = PAYLOAD_SIZE - HEADER_SIZE;
const FIRST: u8 = 1;
const LAST: u8 = 2;
const ABORT: u8 = 4;

/// Message-level failure. Malformed input poisons only the affected publisher's
/// receive state; recover by reattaching after resetting the link generation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MessageError {
    Transport(Error),
    /// Some frames were published before a non-backpressure transport error.
    /// The caller must not resend the consumed prefix.
    PartialTransport {
        error: Error,
        consumed: usize,
        published_slots: usize,
    },
    SendInProgress,
    NoMessageInProgress,
    MessageIdExhausted,
    InputExceedsRemaining,
    UnsupportedVersion,
    InvalidFrame,
    MissingFirst,
    UnexpectedFirst,
    UnexpectedMessageId,
    DestinationChanged,
    InconsistentLength,
    LengthExceeded,
    MissingLast,
    TransferAborted,
    PeerReset,
}

impl From<Error> for MessageError {
    fn from(value: Error) -> Self {
        Self::Transport(value)
    }
}

/// Progress made by a nonblocking write. Retry the unconsumed suffix when
/// `complete` is false; the caller may nudge the lagging READY peer on Full.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SendProgress {
    pub consumed: usize,
    pub published_slots: usize,
    pub complete: bool,
    pub blocked_by: Option<u16>,
}

/// One copied fragment. Assemble fragments with the same `id` from `source`
/// until `complete`; `message_len` is untrusted and requires application policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Fragment {
    pub source: u16,
    pub destination: u16,
    pub id: u64,
    pub message_len: u64,
    pub written: usize,
    pub complete: bool,
}

#[derive(Clone, Copy)]
struct Outgoing {
    destination: u16,
    id: u64,
    len: u64,
    sent: u64,
    published: bool,
}

#[derive(Clone, Copy)]
enum Incoming {
    Idle,
    Receiving {
        id: u64,
        destination: u16,
        len: u64,
        received: u64,
    },
    Failed(MessageError),
}

/// Stateful logical-message endpoint. Send and receive share a single-owner
/// [`Peer`]; call from one task or protect the endpoint with a task-level lock.
/// Messages larger than the ring are streamed without allocation.
pub struct MessageEndpoint<'a> {
    peer: Peer<'a>,
    outgoing: Option<Outgoing>,
    next_id: Option<u64>,
    incoming: [Incoming; MAX_PEERS],
}

impl<'a> MessageEndpoint<'a> {
    /// Consumes a peer so no raw slot operations or second message endpoint
    /// can interleave with this message stream within the link generation.
    pub fn new(peer: Peer<'a>) -> Self {
        Self {
            peer,
            outgoing: None,
            next_id: Some(1),
            incoming: [Incoming::Idle; MAX_PEERS],
        }
    }

    /// Starts a message to one READY peer or to [`crate::BROADCAST`]. Only one
    /// outgoing message can be active at a time, even if destinations differ.
    pub fn start_message(&mut self, destination: u16, len: u64) -> Result<u64, MessageError> {
        if self.outgoing.is_some() {
            return Err(MessageError::SendInProgress);
        }
        self.peer.validate_destination(destination)?;
        let id = self.next_id.ok_or(MessageError::MessageIdExhausted)?;
        self.next_id = id.checked_add(1);
        self.outgoing = Some(Outgoing {
            destination,
            id,
            len,
            sent: 0,
            published: false,
        });
        Ok(id)
    }

    /// Publishes as many frames as space allows, without splitting a fragment.
    /// An empty message needs a call with empty input to publish FIRST|LAST.
    pub fn try_write(&mut self, input: &[u8]) -> Result<SendProgress, MessageError> {
        let mut active = self.outgoing.ok_or(MessageError::NoMessageInProgress)?;
        if input.len() as u128 > (active.len - active.sent) as u128 {
            return Err(MessageError::InputExceedsRemaining);
        }
        let mut progress = SendProgress {
            consumed: 0,
            published_slots: 0,
            complete: false,
            blocked_by: None,
        };
        while progress.consumed < input.len() || (active.len == 0 && !active.published) {
            let bytes = (input.len() - progress.consumed).min(FRAGMENT_SIZE);
            let next_sent = active.sent + bytes as u64;
            let mut frame = [0u8; PAYLOAD_SIZE];
            encode_frame(
                &mut frame,
                active.id,
                active.len,
                (if active.published { 0 } else { FIRST })
                    | (if next_sent == active.len { LAST } else { 0 }),
                &input[progress.consumed..progress.consumed + bytes],
            );
            match self
                .peer
                .try_send(active.destination, &frame[..HEADER_SIZE + bytes])
            {
                Ok(()) => {}
                Err(Error::Full { lagging_peer }) => {
                    progress.blocked_by = Some(lagging_peer);
                    break;
                }
                Err(error) => {
                    self.outgoing = Some(active);
                    return if progress.published_slots == 0 {
                        Err(error.into())
                    } else {
                        Err(MessageError::PartialTransport {
                            error,
                            consumed: progress.consumed,
                            published_slots: progress.published_slots,
                        })
                    };
                }
            }
            active.published = true;
            active.sent = next_sent;
            progress.consumed += bytes;
            progress.published_slots += 1;
            if next_sent == active.len {
                progress.complete = true;
                self.outgoing = None;
                return Ok(progress);
            }
        }
        self.outgoing = Some(active);
        Ok(progress)
    }

    /// Cancels the active message; if frames were published, the ABORT frame
    /// must be sent before another message may begin. Full preserves state.
    pub fn try_abort(&mut self) -> Result<(), MessageError> {
        let active = self.outgoing.ok_or(MessageError::NoMessageInProgress)?;
        if active.published {
            let mut frame = [0u8; PAYLOAD_SIZE];
            encode_frame(&mut frame, active.id, active.len, ABORT, &[]);
            self.peer
                .try_send(active.destination, &frame[..HEADER_SIZE])?;
        }
        self.outgoing = None;
        Ok(())
    }

    /// Reads at most one frame from `publisher` into a fixed-size output.
    /// Call for each READY publisher on every wakeup. Foreign-destination
    /// frames are skipped and their credits advanced by the slot transport.
    /// An invalid frame consumes its slot but poisons this publisher's stream.
    pub fn try_read(
        &mut self,
        publisher: u16,
        output: &mut [u8; FRAGMENT_SIZE],
    ) -> Result<Option<Fragment>, MessageError> {
        let index = publisher as usize;
        if index >= self.peer.peer_count() || index == self.peer.id() {
            return Err(Error::InvalidDestination.into());
        }
        if let Incoming::Failed(error) = self.incoming[index] {
            return Err(error);
        }
        if !self.peer.is_peer_ready(index) {
            if matches!(self.incoming[index], Incoming::Receiving { .. }) {
                self.incoming[index] = Incoming::Failed(MessageError::PeerReset);
                return Err(MessageError::PeerReset);
            }
            return Ok(None);
        }
        let mut slot = [0u8; PAYLOAD_SIZE];
        let Some(message) = self.peer.try_receive(publisher, &mut slot)? else {
            return Ok(None);
        };
        let frame = match decode_frame(&slot[..message.len]) {
            Ok(frame) => frame,
            Err(error) => return self.fail(index, error),
        };
        let state = match self.transition(index, message.destination, &frame) {
            Ok(state) => state,
            Err(error) => return self.fail(index, error),
        };
        self.incoming[index] = state;
        if frame.flags & ABORT != 0 {
            return Err(MessageError::TransferAborted);
        }
        output[..frame.fragment.len()].copy_from_slice(frame.fragment);
        Ok(Some(Fragment {
            source: publisher,
            destination: message.destination,
            id: frame.id,
            message_len: frame.message_len,
            written: frame.fragment.len(),
            complete: frame.flags & LAST != 0,
        }))
    }

    /// Sweeps every publisher after a doorbell (including those without
    /// messages for this peer) so inactive receivers do not hold back credit.
    /// The handler must finish using the fragment before it returns.
    pub fn poll(
        &mut self,
        output: &mut [u8; FRAGMENT_SIZE],
        mut handle: impl FnMut(Fragment, &[u8]),
    ) -> Result<usize, MessageError> {
        let mut count = 0;
        let mut first_error = None;
        for publisher in 0..self.peer.peer_count() {
            if publisher == self.peer.id() {
                continue;
            }
            loop {
                match self.try_read(publisher as u16, output) {
                    Ok(Some(fragment)) => {
                        handle(fragment, &output[..fragment.written]);
                        count += 1;
                    }
                    Ok(None) => break,
                    Err(error) => {
                        first_error.get_or_insert(error);
                        break;
                    }
                }
            }
        }
        first_error.map_or(Ok(count), Err)
    }

    fn transition(
        &self,
        index: usize,
        destination: u16,
        frame: &Frame<'_>,
    ) -> Result<Incoming, MessageError> {
        let (id, len, received) = match self.incoming[index] {
            Incoming::Idle => {
                if frame.flags & FIRST == 0 || frame.flags & ABORT != 0 {
                    return Err(MessageError::MissingFirst);
                }
                (frame.id, frame.message_len, 0)
            }
            Incoming::Receiving {
                id,
                destination: previous_destination,
                len,
                received,
            } => {
                if destination != previous_destination {
                    return Err(MessageError::DestinationChanged);
                }
                if frame.flags & FIRST != 0 {
                    return Err(MessageError::UnexpectedFirst);
                }
                if frame.id != id {
                    return Err(MessageError::UnexpectedMessageId);
                }
                if frame.message_len != len {
                    return Err(MessageError::InconsistentLength);
                }
                (id, len, received)
            }
            Incoming::Failed(error) => return Err(error),
        };
        if frame.flags & ABORT != 0 {
            return Ok(Incoming::Idle);
        }
        let received = received
            .checked_add(frame.fragment.len() as u64)
            .ok_or(MessageError::LengthExceeded)?;
        if received > len || (frame.flags & LAST != 0 && received != len) {
            return Err(MessageError::LengthExceeded);
        }
        if frame.flags & LAST == 0 && received == len {
            return Err(MessageError::MissingLast);
        }
        Ok(if frame.flags & LAST != 0 {
            Incoming::Idle
        } else {
            Incoming::Receiving {
                id,
                destination,
                len,
                received,
            }
        })
    }

    fn fail<T>(&mut self, index: usize, error: MessageError) -> Result<T, MessageError> {
        self.incoming[index] = Incoming::Failed(error);
        Err(error)
    }
}

struct Frame<'a> {
    id: u64,
    message_len: u64,
    flags: u8,
    fragment: &'a [u8],
}

fn encode_frame(output: &mut [u8; PAYLOAD_SIZE], id: u64, len: u64, flags: u8, fragment: &[u8]) {
    output[0] = 1;
    output[1] = flags;
    output[2..4].copy_from_slice(&(HEADER_SIZE as u16).to_le_bytes());
    output[4..8].copy_from_slice(&(fragment.len() as u32).to_le_bytes());
    output[8..16].copy_from_slice(&id.to_le_bytes());
    output[16..24].copy_from_slice(&len.to_le_bytes());
    output[HEADER_SIZE..HEADER_SIZE + fragment.len()].copy_from_slice(fragment);
}

fn decode_frame(slot: &[u8]) -> Result<Frame<'_>, MessageError> {
    if slot.len() < HEADER_SIZE {
        return Err(MessageError::InvalidFrame);
    }
    if slot[0] != 1 {
        return Err(MessageError::UnsupportedVersion);
    }
    let flags = slot[1];
    let header_len = u16::from_le_bytes([slot[2], slot[3]]) as usize;
    let fragment_len = u32::from_le_bytes([slot[4], slot[5], slot[6], slot[7]]) as usize;
    let id = u64::from_le_bytes(
        slot[8..16]
            .try_into()
            .map_err(|_| MessageError::InvalidFrame)?,
    );
    let message_len = u64::from_le_bytes(
        slot[16..24]
            .try_into()
            .map_err(|_| MessageError::InvalidFrame)?,
    );
    if id == 0
        || header_len != HEADER_SIZE
        || flags & !(FIRST | LAST | ABORT) != 0
        || fragment_len > FRAGMENT_SIZE
        || fragment_len != slot.len() - HEADER_SIZE
        || (flags & ABORT != 0 && (flags != ABORT || fragment_len != 0))
        || (flags & ABORT == 0
            && ((message_len == 0 && (flags != (FIRST | LAST) || fragment_len != 0))
                || (message_len != 0 && fragment_len == 0)))
    {
        return Err(MessageError::InvalidFrame);
    }
    Ok(Frame {
        id,
        message_len,
        flags,
        fragment: &slot[HEADER_SIZE..],
    })
}

#[cfg(test)]
mod tests {
    use core::sync::atomic::{AtomicU32, Ordering};

    use super::*;
    use crate::{READY, Section};

    #[test]
    fn message_v1_streams_across_ring_wrap_and_separates_publishers() {
        let sections = [Section::new(), Section::new(), Section::new()];
        let states = [const { AtomicU32::new(0) }; 3];
        // SAFETY: The sections have distinct writers and stable backing.
        let mut peers = [
            unsafe { Peer::attach(0, &sections, &states).unwrap() },
            unsafe { Peer::attach(1, &sections, &states).unwrap() },
            unsafe { Peer::attach(2, &sections, &states).unwrap() },
        ];
        for (id, peer) in peers.iter_mut().enumerate() {
            peer.initialize().unwrap();
            states[id].store(READY, Ordering::Release);
        }
        let [first, second, third] = peers;
        let mut sender = MessageEndpoint::new(first);
        let mut other_sender = MessageEndpoint::new(third);
        let mut receiver = MessageEndpoint::new(second);
        let payload = [0x91; FRAGMENT_SIZE * (crate::CAPACITY + 2)];
        sender.start_message(1, payload.len() as u64).unwrap();
        let initial = sender.try_write(&payload).unwrap();
        assert_eq!(initial.published_slots, crate::CAPACITY);
        assert!(!initial.complete);
        assert_eq!(initial.blocked_by, Some(1));
        other_sender.start_message(1, 3).unwrap();
        assert!(other_sender.try_write(b"two").unwrap().complete);
        let mut output = [0u8; FRAGMENT_SIZE];
        let unrelated = receiver.try_read(2, &mut output).unwrap().unwrap();
        assert_eq!(&output[..unrelated.written], b"two");
        let mut collected = std::vec::Vec::new();
        for _ in 0..crate::CAPACITY {
            let part = receiver.try_read(0, &mut output).unwrap().unwrap();
            collected.extend_from_slice(&output[..part.written]);
            assert!(!part.complete);
        }
        let blocked = sender.try_write(&payload[initial.consumed..]).unwrap();
        assert_eq!(blocked.blocked_by, Some(2));
        assert_eq!(blocked.consumed, 0);
        assert_eq!(other_sender.poll(&mut output, |_, _| {}).unwrap(), 0);
        let final_write = sender.try_write(&payload[initial.consumed..]).unwrap();
        assert!(final_write.complete);
        for _ in 0..2 {
            let part = receiver.try_read(0, &mut output).unwrap().unwrap();
            collected.extend_from_slice(&output[..part.written]);
            if part.complete {
                assert_eq!(part.message_len, payload.len() as u64);
            }
        }
        assert_eq!(collected, payload);

        sender.start_message(1, 0).unwrap();
        assert!(sender.try_write(&[]).unwrap().complete);
        let empty = receiver.try_read(0, &mut output).unwrap().unwrap();
        assert_eq!(
            (empty.written, empty.message_len, empty.complete),
            (0, 0, true)
        );

        sender.start_message(1, 400).unwrap();
        sender.try_write(b"partial").unwrap();
        assert!(!receiver.try_read(0, &mut output).unwrap().unwrap().complete);
        sender.try_abort().unwrap();
        assert_eq!(
            receiver.try_read(0, &mut output),
            Err(MessageError::TransferAborted)
        );
        sender.start_message(1, 2).unwrap();
        assert!(sender.try_write(b"ok").unwrap().complete);
        let next = receiver.try_read(0, &mut output).unwrap().unwrap();
        assert_eq!(&output[..next.written], b"ok");
    }

    #[test]
    fn malformed_publisher_does_not_prevent_other_publishers_from_progressing() {
        let sections = [Section::new(), Section::new(), Section::new()];
        let states = [const { AtomicU32::new(0) }; 3];
        // SAFETY: Distinct output owners and stable backing for the test.
        let mut peers = [
            unsafe { Peer::attach(0, &sections, &states).unwrap() },
            unsafe { Peer::attach(1, &sections, &states).unwrap() },
            unsafe { Peer::attach(2, &sections, &states).unwrap() },
        ];
        for (id, peer) in peers.iter_mut().enumerate() {
            peer.initialize().unwrap();
            states[id].store(READY, Ordering::Release);
        }
        let [mut bad_sender, receiver, good_sender] = peers;
        bad_sender.try_send(1, &[0]).unwrap();
        let mut good_sender = MessageEndpoint::new(good_sender);
        good_sender.start_message(1, 2).unwrap();
        good_sender.try_write(b"ok").unwrap();
        let mut receiver = MessageEndpoint::new(receiver);
        let mut output = [0; FRAGMENT_SIZE];
        let mut received = std::vec::Vec::new();
        assert_eq!(
            receiver.poll(&mut output, |fragment, payload| {
                assert_eq!(fragment.source, 2);
                received.extend_from_slice(payload);
            }),
            Err(MessageError::InvalidFrame)
        );
        assert_eq!(received, b"ok");
        assert_eq!(
            receiver.try_read(0, &mut output),
            Err(MessageError::InvalidFrame)
        );
    }

    #[test]
    fn v1_frame_preserves_existing_wire_bytes_and_rejects_malformed_input() {
        let mut slot = [0u8; PAYLOAD_SIZE];
        encode_frame(&mut slot, 0x0807_0605_0403_0201, 3, FIRST | LAST, b"abc");
        assert_eq!(
            &slot[..27],
            &[
                1, 3, 24, 0, 3, 0, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8, 3, 0, 0, 0, 0, 0, 0, 0, b'a',
                b'b', b'c'
            ]
        );
        assert_eq!(decode_frame(&slot[..27]).unwrap().fragment, b"abc");
        slot[1] = 0x80;
        assert!(matches!(
            decode_frame(&slot[..27]),
            Err(MessageError::InvalidFrame)
        ));
    }
}
