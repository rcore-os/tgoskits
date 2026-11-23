#![cfg_attr(feature = "arceos", no_main)]
#![cfg_attr(feature = "arceos", no_std)]

#[cfg(feature = "arceos")]
use ax_std as _;

#[cfg_attr(feature = "arceos", unsafe(no_mangle))]
#[cfg(feature = "arceos")]
fn main() {
    subscriber::run();
}

#[cfg(not(feature = "arceos"))]
fn main() {}

#[cfg(feature = "arceos")]
mod subscriber {
    use core::result::Result::{Err, Ok};

    use arceos_ivshmem_pci::{EVENT_VECTOR_0, IvshmemDevice, IvshmemLayout};
    use ax_std::{
        format, println,
        string::{String, ToString},
        thread,
        time::{Duration, Instant},
    };
    use axivc::{IvcMessageReceiver, IvcMessageSender, IvshmemMessagePort, IvshmemSessionState};

    const ACK_BODY: &[u8] = b"ack from arceos subscriber";
    const APP_HEADER_LEN: usize = 11;
    // Exercise single-fragment and full-ring payload boundaries.
    const APP_MAX_MESSAGE_LEN: usize = 7484;
    const ACK_MESSAGE_LEN: usize = APP_HEADER_LEN + ACK_BODY.len();
    const DATA_MESSAGE_LENGTHS: [usize; 3] = [233, 7425, 7484];
    const REQUEST_MESSAGE_LENGTHS: [usize; 5] = [231, 232, 233, 7424, 7484];
    const PUBLISH_COUNT: u64 = REQUEST_MESSAGE_LENGTHS.len() as u64;
    const SUBSCRIBE_DATA_COUNT: u64 = DATA_MESSAGE_LENGTHS.len() as u64;
    const IVSHMEM_PEER_ID: u32 = 1;
    const IVSHMEM_PEER_SLOT: u32 = 0;
    const SESSION_TIMEOUT: Duration = Duration::from_secs(30);
    const TRANSFER_TIMEOUT: Duration = Duration::from_secs(30);
    const EVENT_POLL_DELAY: Duration = Duration::from_millis(1);

    pub fn run() {
        if let Err(error) = run_inner() {
            println!("ivc session failed backend=ivshmem-pci peer={IVSHMEM_PEER_ID}: {error}");
        }
    }

    fn run_inner() -> Result<(), String> {
        let device =
            IvshmemDevice::discover(IvshmemLayout::two_peer()).map_err(|e| e.to_string())?;
        if device.peer_id() != IVSHMEM_PEER_ID || device.max_peers() != 2 {
            return Err(format!(
                "unexpected ivshmem profile peer={} max_peers={}",
                device.peer_id(),
                device.max_peers()
            ));
        }
        println!(
            "ivc subscriber backend=ivshmem-pci peer={} max_peers={} event_mode=event-status-poll \
             msi_x=unavailable",
            device.peer_id(),
            device.max_peers()
        );

        let local_output = device.own_output().map_err(|e| e.to_string())?;
        let peer_output = device
            .output(IVSHMEM_PEER_SLOT)
            .map_err(|e| e.to_string())?;
        // SAFETY: the device owns both BAR2 mappings until this function
        // returns. Its page-aligned output sections are exposed only to this
        // protocol; the peer uses the reciprocal local/peer arrangement, and
        // no endpoint from an earlier session remains in this single run.
        let mut port = unsafe {
            IvshmemMessagePort::attach(
                local_output.as_ptr(),
                local_output.len(),
                peer_output.as_const_ptr(),
                peer_output.len(),
            )
        }
        .map_err(|e| e.to_string())?;
        // SAFETY: this app is the sole initializer of its output section for
        // this guest execution, and no endpoint has been created yet.
        let local_incarnation = unsafe { port.begin_session() }.map_err(|e| e.to_string())?;
        println!(
            "ivc session marker=waiting backend=ivshmem-pci role=subscriber peer={} \
             local_incarnation={local_incarnation} phase=after-publisher",
            device.peer_id()
        );

        wait_for_session(&device, &mut port, local_incarnation)?;
        let (sender, receiver) = port.try_connect().map_err(|e| e.to_string())?.into_parts();
        let mut sender = DuplexSender::new(sender);
        let mut receiver = RequestReceiver::new(receiver);
        let started = Instant::now();
        let mut event_observed = false;

        while !sender.is_complete() || !receiver.is_complete() {
            let received = receiver.poll()?;
            let sent = sender.poll(receiver.highest_received())?;
            if sent || received {
                ring_peer(&device)?;
            }
            event_observed |= poll_events(&device, event_observed)?;

            if !port.is_session_current() {
                return Err("ivshmem Message V1 session was lost".into());
            }
            if sender.is_complete() && receiver.is_complete() {
                break;
            }
            if started.elapsed() >= TRANSFER_TIMEOUT {
                return Err(format!(
                    "full-duplex exchange timed out sent_data={} acked={} received_requests={}",
                    sender.data_count(),
                    sender.ack_count(),
                    receiver.request_count()
                ));
            }
            if !sent && !received {
                thread::sleep(EVENT_POLL_DELAY);
            }
        }

        println!(
            "ivc session marker=complete backend=ivshmem-pci role=subscriber peer={}",
            device.peer_id()
        );
        println!("ivc subscriber full-duplex demo complete backend=ivshmem-pci");
        Ok(())
    }

    fn wait_for_session(
        device: &IvshmemDevice,
        port: &mut IvshmemMessagePort<'_>,
        local_incarnation: core::num::NonZeroU32,
    ) -> Result<(), String> {
        let started = Instant::now();
        let mut event_observed = false;
        loop {
            match port.poll_handshake().map_err(|e| e.to_string())? {
                IvshmemSessionState::Ready { local, peer } => {
                    if local != local_incarnation {
                        return Err(format!(
                            "session ready with unexpected local incarnation {local}"
                        ));
                    }
                    println!(
                        "ivc session marker=ready backend=ivshmem-pci role=subscriber peer={} \
                         local_incarnation={local} peer_incarnation={peer}",
                        device.peer_id()
                    );
                    return Ok(());
                }
                IvshmemSessionState::WaitingForPeer { .. }
                | IvshmemSessionState::WaitingForPeerAcknowledgement { .. } => {}
                state => return Err(format!("session left the waiting path: {state:?}")),
            }
            event_observed |= poll_events(device, event_observed)?;
            if started.elapsed() >= SESSION_TIMEOUT {
                return Err("peer did not join the ivshmem Message V1 session".into());
            }
            thread::sleep(EVENT_POLL_DELAY);
        }
    }

    fn poll_events(device: &IvshmemDevice, already_observed: bool) -> Result<bool, String> {
        if device
            .take_events(EVENT_VECTOR_0)
            .map_err(|e| e.to_string())?
            == 0
        {
            return Ok(false);
        }
        if !already_observed {
            println!("ivc session event backend=ivshmem-pci source=event-status");
        }
        Ok(true)
    }

    fn ring_peer(device: &IvshmemDevice) -> Result<(), String> {
        device
            .ring_doorbell(IVSHMEM_PEER_SLOT, 0)
            .map_err(|e| e.to_string())
    }

    struct DuplexSender<'a> {
        sender: IvcMessageSender<'a>,
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
        fn new(sender: IvcMessageSender<'a>) -> Self {
            Self {
                sender,
                payload: [0; APP_MAX_MESSAGE_LEN],
                active: None,
                consumed: 0,
                data_sent: 0,
                last_acked: 0,
            }
        }

        fn poll(&mut self, highest_received: u64) -> Result<bool, String> {
            if self.is_complete() {
                return Ok(false);
            }
            if self.active.is_none() && !self.start_next(highest_received)? {
                return Ok(false);
            }
            let active = self.active.ok_or("missing active subscriber message")?;

            let mut wrote = false;
            loop {
                let progress = self
                    .sender
                    .try_write(&self.payload[self.consumed..active.len()])
                    .map_err(|e| format!("{e:?}"))?;
                self.consumed += progress.consumed();
                wrote |= progress.published_slots() > 0;
                if progress.is_complete() {
                    match active {
                        ActiveSend::Data { sequence, len } => {
                            println!("ivc send data seq={sequence} len={len}");
                            self.data_sent = sequence;
                        }
                        ActiveSend::Ack { sequence, .. } => {
                            println!("ivc ack pub seq={sequence}");
                            self.last_acked = sequence;
                        }
                    }
                    self.active = None;
                    self.consumed = 0;
                    return Ok(true);
                }
                if progress.consumed() == 0 {
                    return Ok(wrote);
                }
            }
        }

        fn start_next(&mut self, highest_received: u64) -> Result<bool, String> {
            if self.data_sent < SUBSCRIBE_DATA_COUNT {
                let index = self.data_sent as usize;
                let sequence = self.data_sent + 1;
                let message_len = DATA_MESSAGE_LENGTHS[index];
                if !encode_pattern_message(
                    &mut self.payload[..message_len],
                    AppMessageKind::Data,
                    sequence,
                ) {
                    return Err(format!("cannot encode data seq={sequence}"));
                }
                self.sender
                    .start_message(message_len as u64)
                    .map_err(|e| format!("{e:?}"))?;
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
            let ack = encode_ack(sequence);
            self.payload[..ACK_MESSAGE_LEN].copy_from_slice(&ack);
            self.sender
                .start_message(ACK_MESSAGE_LEN as u64)
                .map_err(|e| format!("{e:?}"))?;
            self.consumed = 0;
            self.active = Some(ActiveSend::Ack {
                sequence,
                len: ACK_MESSAGE_LEN,
            });
            Ok(true)
        }

        const fn is_complete(&self) -> bool {
            self.data_sent == SUBSCRIBE_DATA_COUNT && self.last_acked == PUBLISH_COUNT
        }

        const fn data_count(&self) -> u64 {
            self.data_sent
        }

        const fn ack_count(&self) -> u64 {
            self.last_acked
        }
    }

    struct RequestReceiver<'a> {
        receiver: IvcMessageReceiver<'a>,
        payload: [u8; APP_MAX_MESSAGE_LEN],
        received: usize,
        expected_sequence: u64,
        highest_received: u64,
        complete: bool,
    }

    impl<'a> RequestReceiver<'a> {
        fn new(receiver: IvcMessageReceiver<'a>) -> Self {
            Self {
                receiver,
                payload: [0; APP_MAX_MESSAGE_LEN],
                received: 0,
                expected_sequence: 1,
                highest_received: 0,
                complete: false,
            }
        }

        fn poll(&mut self) -> Result<bool, String> {
            if self.complete {
                return Ok(false);
            }
            let mut progressed = false;
            loop {
                if self.received == 0 {
                    match self
                        .receiver
                        .peek_message_meta()
                        .map_err(|e| format!("{e:?}"))?
                    {
                        Some(meta) if message_fits(meta.len(), self.payload.len()) => {}
                        Some(meta) => {
                            return Err(format!("oversized request len={}", meta.len()));
                        }
                        None => return Ok(progressed),
                    }
                }

                let progress = self
                    .receiver
                    .try_read(&mut self.payload[self.received..])
                    .map_err(|e| format!("{e:?}"))?;
                self.received += progress.written();
                progressed |= progress.consumed_slots() > 0;
                if progress.is_complete() {
                    self.validate_request()?;
                    self.received = 0;
                    if self.complete {
                        return Ok(true);
                    }
                } else if progress.consumed_slots() == 0 {
                    return Ok(progressed);
                }
            }
        }

        fn validate_request(&mut self) -> Result<(), String> {
            let message = decode_app_message(&self.payload[..self.received])
                .ok_or("malformed application request payload")?;
            let expected_len = *REQUEST_MESSAGE_LENGTHS
                .get((self.expected_sequence - 1) as usize)
                .ok_or_else(|| format!("unexpected request seq={}", message.sequence))?;
            if !validate_pattern_message(
                &message,
                AppMessageKind::Request,
                self.expected_sequence,
                expected_len,
            ) {
                return Err(format!(
                    "request expected={} actual={} len={}",
                    self.expected_sequence, message.sequence, self.received
                ));
            }
            let text = core::str::from_utf8(message.body).unwrap_or("<non-utf8>");
            println!(
                "ivc recv request seq={} len={} msg={text}",
                message.sequence, self.received
            );
            self.highest_received = self.expected_sequence;
            self.expected_sequence += 1;
            if self.expected_sequence == PUBLISH_COUNT + 1 {
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

    fn encode_ack(sequence: u64) -> [u8; ACK_MESSAGE_LEN] {
        let mut payload = [0u8; ACK_MESSAGE_LEN];
        encode_app_header(
            &mut payload,
            AppMessageKind::Ack,
            sequence,
            ACK_BODY.len() as u16,
        );
        payload[APP_HEADER_LEN..].copy_from_slice(ACK_BODY);
        payload
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
}
