#![no_std]

//! Application-level Message V1 subscriber state machines.
//!
//! This crate validates the publisher requests and drives the subscriber's
//! patterned data and acknowledgement messages over a logical
//! [`MessageRecordEndpoint`]. Device discovery, Message V1, doorbells, waiting,
//! and session lifetime remain owned by the endpoint implementation.

use core::fmt;

pub use arceos_ivshmem_pci::{MessageRecordEndpoint, RecordProgress};

const APP_HEADER_LEN: usize = 11;
// Exercise single-fragment and full-ring payload boundaries.
const APP_MAX_MESSAGE_LEN: usize = 7484;
const DATA_MESSAGE_LENGTHS: [usize; 3] = [233, 7425, 7484];
const REQUEST_MESSAGE_LENGTHS: [usize; 5] = [231, 232, 233, 7424, 7484];
const REQUEST_COUNT: u64 = REQUEST_MESSAGE_LENGTHS.len() as u64;
const DATA_COUNT: u64 = DATA_MESSAGE_LENGTHS.len() as u64;
const ACK_BODY_MAX_LEN: usize = APP_MAX_MESSAGE_LEN - APP_HEADER_LEN;

/// Progress and completion reported by one nonblocking subscriber poll.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PollOutcome {
    progressed: bool,
    complete: bool,
}

impl PollOutcome {
    const fn new(progressed: bool, complete: bool) -> Self {
        Self {
            progressed,
            complete,
        }
    }

    /// Returns whether this poll consumed or published any Message V1 slot.
    pub const fn progressed(self) -> bool {
        self.progressed
    }

    /// Returns whether all requests were received and every data/ack message
    /// was fully published.
    pub const fn is_complete(self) -> bool {
        self.complete
    }
}

/// Runtime-visible application events completed by one subscriber poll.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MessageDemoEvent<'a> {
    /// A publisher request was received and validated.
    RequestReceived {
        /// One-based publisher request sequence.
        sequence: u64,
        /// Complete application payload length, including the header.
        len: usize,
        /// Request body borrowed for this callback only.
        body: &'a [u8],
    },
    /// A subscriber data message reached the peer.
    DataSent {
        /// One-based data sequence.
        sequence: u64,
        /// Complete application payload length, including the header.
        len: usize,
    },
    /// A subscriber acknowledgement reached the peer.
    AckSent {
        /// One-based request sequence acknowledged by this message.
        sequence: u64,
    },
}

/// Errors from subscriber construction or application-message validation.
#[derive(Debug)]
pub enum MessageDemoError<E> {
    /// The underlying Message V1 endpoint failed.
    Transport(E),
    /// The runtime-provided acknowledgement body cannot fit in one demo
    /// application message.
    AckBodyTooLong {
        /// Supplied acknowledgement body length.
        len: usize,
        /// Maximum body length accepted by this demo protocol.
        max: usize,
    },
    /// A request declared a transport payload larger than the demo maximum.
    OversizedRequest {
        /// Declared request payload length.
        len: u64,
    },
    /// The application header or declared body length is malformed.
    MalformedRequest,
    /// An internal send transition cannot represent the configured demo
    /// message.
    InvalidSendState,
    /// The request kind, sequence, length, or body pattern did not match the
    /// next expected publisher message.
    InvalidRequest {
        /// Next expected one-based request sequence.
        expected: u64,
        /// Sequence encoded by the received request.
        actual: u64,
        /// Complete received payload length.
        len: usize,
    },
}

impl<E: fmt::Display> fmt::Display for MessageDemoError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport(error) => error.fmt(formatter),
            Self::AckBodyTooLong { len, max } => {
                write!(formatter, "ack body length {len} exceeds maximum {max}")
            }
            Self::OversizedRequest { len } => write!(formatter, "oversized request len={len}"),
            Self::MalformedRequest => formatter.write_str("malformed application request payload"),
            Self::InvalidSendState => formatter.write_str("subscriber send state is inconsistent"),
            Self::InvalidRequest {
                expected,
                actual,
                len,
            } => write!(
                formatter,
                "request expected={expected} actual={actual} len={len}"
            ),
        }
    }
}

impl<E> core::error::Error for MessageDemoError<E>
where
    E: core::error::Error + 'static,
{
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Transport(error) => Some(error),
            _ => None,
        }
    }
}

/// Stateful, nonblocking subscriber for the Linux/ArceOS Message V1 demo.
///
/// Construct it from a logical device endpoint and the acknowledgement body
/// required by the peer. Every event is delivered through the callback supplied
/// to [`Self::poll`], so the runtime retains ownership of console output and
/// other platform-specific side effects.
pub struct MessageDemoSubscriber<'a, E> {
    endpoint: E,
    sender: DuplexSender<'a>,
    receiver: RequestReceiver,
}

impl<'a, E> MessageDemoSubscriber<'a, E>
where
    E: MessageRecordEndpoint,
{
    /// Creates the two application state machines around one device endpoint.
    ///
    /// `ack_body` is copied into each acknowledgement message and must live for
    /// at least as long as the subscriber.
    ///
    /// # Errors
    ///
    /// Returns [`MessageDemoError::AckBodyTooLong`] when the body cannot fit in
    /// one demo application message.
    pub fn new(endpoint: E, ack_body: &'a [u8]) -> Result<Self, MessageDemoError<E::Error>> {
        if ack_body.len() > ACK_BODY_MAX_LEN {
            return Err(MessageDemoError::AckBodyTooLong {
                len: ack_body.len(),
                max: ACK_BODY_MAX_LEN,
            });
        }

        Ok(Self {
            endpoint,
            sender: DuplexSender::new(ack_body),
            receiver: RequestReceiver::new(),
        })
    }

    /// Advances both directions without blocking.
    ///
    /// Each completed request, data message, and acknowledgement is reported
    /// through `on_event`. The callback may print the marker appropriate for
    /// its runtime. It must not retain the borrowed request body.
    ///
    /// # Errors
    ///
    /// Returns a transport error for a failed Message V1 operation and
    /// [`MessageDemoError::InvalidRequest`] for a mismatched request sequence,
    /// length, or pattern.
    pub fn poll<F>(&mut self, mut on_event: F) -> Result<PollOutcome, MessageDemoError<E::Error>>
    where
        F: for<'event> FnMut(MessageDemoEvent<'event>),
    {
        let received = self.receiver.poll(&mut self.endpoint, &mut on_event)?;
        let sent = self.sender.poll(
            &mut self.endpoint,
            self.receiver.highest_received(),
            &mut on_event,
        )?;
        Ok(PollOutcome::new(
            received || sent,
            self.receiver.is_complete() && self.sender.is_complete(),
        ))
    }

    /// Waits for device or peer progress after an idle poll.
    ///
    /// Returns whether a device event was observed.
    pub fn wait_for_progress(&mut self) -> Result<bool, MessageDemoError<E::Error>> {
        self.endpoint
            .wait_for_progress()
            .map_err(MessageDemoError::Transport)
    }

    /// Returns whether the peer consumed every published data and ACK slot.
    ///
    /// Call after [`Self::is_complete`] so no local record is still active and
    /// the endpoint's receive credit represents final delivery.
    pub fn outbound_drained(&mut self) -> Result<bool, MessageDemoError<E::Error>> {
        self.endpoint
            .outbound_drained()
            .map_err(MessageDemoError::Transport)
    }

    /// Returns whether both application directions are complete.
    pub const fn is_complete(&self) -> bool {
        self.receiver.is_complete() && self.sender.is_complete()
    }

    /// Returns the number of fully published subscriber data messages.
    pub const fn data_count(&self) -> u64 {
        self.sender.data_sent
    }

    /// Returns the number of fully published acknowledgements.
    pub const fn ack_count(&self) -> u64 {
        self.sender.last_acked
    }

    /// Returns the number of validated publisher requests.
    pub const fn request_count(&self) -> u64 {
        self.receiver.request_count()
    }
}

struct DuplexSender<'a> {
    ack_body: &'a [u8],
    payload: [u8; APP_MAX_MESSAGE_LEN],
    active: Option<ActiveSend>,
    consumed: usize,
    data_sent: u64,
    last_acked: u64,
}

#[derive(Clone, Copy)]
enum ActiveSend {
    Data { sequence: u64, len: usize },
    Ack { sequence: u64, len: usize },
}

impl ActiveSend {
    const fn len(self) -> usize {
        match self {
            Self::Data { len, .. } | Self::Ack { len, .. } => len,
        }
    }
}

impl<'a> DuplexSender<'a> {
    fn new(ack_body: &'a [u8]) -> Self {
        Self {
            ack_body,
            payload: [0; APP_MAX_MESSAGE_LEN],
            active: None,
            consumed: 0,
            data_sent: 0,
            last_acked: 0,
        }
    }

    fn poll<E, F>(
        &mut self,
        endpoint: &mut E,
        highest_received: u64,
        on_event: &mut F,
    ) -> Result<bool, MessageDemoError<E::Error>>
    where
        E: MessageRecordEndpoint,
        F: for<'event> FnMut(MessageDemoEvent<'event>),
    {
        if self.is_complete() {
            return Ok(false);
        }
        if self.active.is_none() && !self.start_next(endpoint, highest_received)? {
            return Ok(false);
        }
        let active = self.active.ok_or(MessageDemoError::InvalidSendState)?;

        let mut wrote = false;
        loop {
            let progress = endpoint
                .write_record(&self.payload[self.consumed..active.len()])
                .map_err(MessageDemoError::Transport)?;
            self.consumed += progress.transferred();
            wrote |= progress.progressed();
            if progress.is_complete() {
                match active {
                    ActiveSend::Data { sequence, len } => {
                        self.data_sent = sequence;
                        on_event(MessageDemoEvent::DataSent { sequence, len });
                    }
                    ActiveSend::Ack { sequence, .. } => {
                        self.last_acked = sequence;
                        on_event(MessageDemoEvent::AckSent { sequence });
                    }
                }
                self.active = None;
                self.consumed = 0;
                return Ok(true);
            }
            if !progress.progressed() {
                return Ok(wrote);
            }
        }
    }

    fn start_next<E>(
        &mut self,
        endpoint: &mut E,
        highest_received: u64,
    ) -> Result<bool, MessageDemoError<E::Error>>
    where
        E: MessageRecordEndpoint,
    {
        if self.data_sent < DATA_COUNT {
            let sequence = self.data_sent + 1;
            let message_len = *DATA_MESSAGE_LENGTHS
                .get(self.data_sent as usize)
                .ok_or(MessageDemoError::InvalidSendState)?;
            if !encode_pattern_message(
                &mut self.payload[..message_len],
                AppMessageKind::Data,
                sequence,
            ) {
                return Err(MessageDemoError::InvalidSendState);
            }
            endpoint
                .start_record(message_len as u64)
                .map_err(MessageDemoError::Transport)?;
            self.consumed = 0;
            self.active = Some(ActiveSend::Data {
                sequence,
                len: message_len,
            });
            return Ok(true);
        }

        if self.last_acked >= highest_received {
            return Ok(false);
        }
        let sequence = self.last_acked + 1;
        let message_len = APP_HEADER_LEN + self.ack_body.len();
        encode_ack(&mut self.payload[..message_len], sequence, self.ack_body);
        endpoint
            .start_record(message_len as u64)
            .map_err(MessageDemoError::Transport)?;
        self.consumed = 0;
        self.active = Some(ActiveSend::Ack {
            sequence,
            len: message_len,
        });
        Ok(true)
    }

    const fn is_complete(&self) -> bool {
        self.data_sent == DATA_COUNT && self.last_acked == REQUEST_COUNT
    }
}

struct RequestReceiver {
    payload: [u8; APP_MAX_MESSAGE_LEN],
    received: usize,
    expected_sequence: u64,
    highest_received: u64,
    complete: bool,
}

impl RequestReceiver {
    fn new() -> Self {
        Self {
            payload: [0; APP_MAX_MESSAGE_LEN],
            received: 0,
            expected_sequence: 1,
            highest_received: 0,
            complete: false,
        }
    }

    fn poll<E, F>(
        &mut self,
        endpoint: &mut E,
        on_event: &mut F,
    ) -> Result<bool, MessageDemoError<E::Error>>
    where
        E: MessageRecordEndpoint,
        F: for<'event> FnMut(MessageDemoEvent<'event>),
    {
        if self.complete {
            return Ok(false);
        }
        let mut progressed = false;
        loop {
            if self.received == 0 {
                match endpoint
                    .peek_record_len()
                    .map_err(MessageDemoError::Transport)?
                {
                    Some(len) if message_fits(len, self.payload.len()) => {}
                    Some(len) => {
                        return Err(MessageDemoError::OversizedRequest { len });
                    }
                    None => return Ok(progressed),
                }
            }

            let progress = endpoint
                .read_record(&mut self.payload[self.received..])
                .map_err(MessageDemoError::Transport)?;
            self.received += progress.transferred();
            progressed |= progress.progressed();
            if progress.is_complete() {
                self.validate_request::<F, E>(on_event)?;
                self.received = 0;
                if self.complete {
                    return Ok(true);
                }
            } else if !progress.progressed() {
                return Ok(progressed);
            }
        }
    }

    fn validate_request<F, E>(&mut self, on_event: &mut F) -> Result<(), MessageDemoError<E::Error>>
    where
        E: MessageRecordEndpoint,
        F: for<'event> FnMut(MessageDemoEvent<'event>),
    {
        let message = decode_app_message(&self.payload[..self.received])
            .ok_or(MessageDemoError::MalformedRequest)?;
        let expected_len = *REQUEST_MESSAGE_LENGTHS
            .get((self.expected_sequence - 1) as usize)
            .ok_or(MessageDemoError::InvalidRequest {
                expected: self.expected_sequence,
                actual: message.sequence,
                len: self.received,
            })?;
        if !validate_pattern_message(
            &message,
            AppMessageKind::Request,
            self.expected_sequence,
            expected_len,
        ) {
            return Err(MessageDemoError::InvalidRequest {
                expected: self.expected_sequence,
                actual: message.sequence,
                len: self.received,
            });
        }

        on_event(MessageDemoEvent::RequestReceived {
            sequence: message.sequence,
            len: self.received,
            body: message.body,
        });
        self.highest_received = self.expected_sequence;
        self.expected_sequence += 1;
        if self.expected_sequence == REQUEST_COUNT + 1 {
            self.complete = true;
        }
        Ok(())
    }

    const fn is_complete(&self) -> bool {
        self.complete
    }

    const fn highest_received(&self) -> u64 {
        self.highest_received
    }

    const fn request_count(&self) -> u64 {
        self.expected_sequence - 1
    }
}

fn encode_pattern_message(payload: &mut [u8], kind: AppMessageKind, sequence: u64) -> bool {
    let Some(body_len) = payload.len().checked_sub(APP_HEADER_LEN) else {
        return false;
    };
    let Ok(body_len) = u16::try_from(body_len) else {
        return false;
    };
    encode_app_header(payload, kind, sequence, body_len);
    for (index, byte) in payload[APP_HEADER_LEN..].iter_mut().enumerate() {
        *byte = pattern_byte(kind, sequence, index);
    }
    true
}

fn validate_pattern_message(
    message: &AppMessage<'_>,
    expected_kind: AppMessageKind,
    expected_sequence: u64,
    expected_len: usize,
) -> bool {
    message.kind == expected_kind
        && message.sequence == expected_sequence
        && APP_HEADER_LEN + message.body.len() == expected_len
        && message
            .body
            .iter()
            .enumerate()
            .all(|(index, &byte)| byte == pattern_byte(expected_kind, expected_sequence, index))
}

fn pattern_byte(kind: AppMessageKind, sequence: u64, index: usize) -> u8 {
    let pattern_index = (index as u8)
        .wrapping_add(sequence as u8)
        .wrapping_add((kind as u8).wrapping_mul(7));
    b'a' + pattern_index % 26
}

fn encode_ack(payload: &mut [u8], sequence: u64, body: &[u8]) {
    encode_app_header(payload, AppMessageKind::Ack, sequence, body.len() as u16);
    payload[APP_HEADER_LEN..].copy_from_slice(body);
}

fn encode_app_header(payload: &mut [u8], kind: AppMessageKind, sequence: u64, body_len: u16) {
    payload[0] = kind as u8;
    payload[1..9].copy_from_slice(&sequence.to_le_bytes());
    payload[9..APP_HEADER_LEN].copy_from_slice(&body_len.to_le_bytes());
}

fn message_fits(message_len: u64, capacity: usize) -> bool {
    usize::try_from(message_len).is_ok_and(|len| len <= capacity)
}

fn decode_app_message(payload: &[u8]) -> Option<AppMessage<'_>> {
    if payload.len() < APP_HEADER_LEN {
        return None;
    }
    let kind = AppMessageKind::from_raw(payload[0])?;
    let sequence = u64::from_le_bytes(payload[1..9].try_into().ok()?);
    let body_len = u16::from_le_bytes(payload[9..APP_HEADER_LEN].try_into().ok()?) as usize;
    let body_end = APP_HEADER_LEN.checked_add(body_len)?;
    if body_end != payload.len() {
        return None;
    }
    Some(AppMessage {
        kind,
        sequence,
        body: &payload[APP_HEADER_LEN..body_end],
    })
}

#[derive(Clone, Copy, Eq, PartialEq)]
#[repr(u8)]
enum AppMessageKind {
    Request = 1,
    Ack     = 2,
    Data    = 3,
}

impl AppMessageKind {
    const fn from_raw(raw: u8) -> Option<Self> {
        match raw {
            1 => Some(Self::Request),
            2 => Some(Self::Ack),
            3 => Some(Self::Data),
            _ => None,
        }
    }
}

struct AppMessage<'a> {
    kind: AppMessageKind,
    sequence: u64,
    body: &'a [u8],
}
