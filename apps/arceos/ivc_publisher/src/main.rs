#![cfg_attr(feature = "arceos", no_main)]
#![cfg_attr(feature = "arceos", no_std)]

#[cfg(feature = "arceos")]
use ax_std as _;

#[cfg_attr(feature = "arceos", unsafe(no_mangle))]
#[cfg(feature = "arceos")]
fn main() {
    publisher::run();
}

#[cfg(not(feature = "arceos"))]
fn main() {}

#[cfg(feature = "arceos")]
mod publisher {
    use arceos_ivshmem_pci::{
        IvcDevice, IvcEndpoint, IvcError, IvcPeerRole, MessageRecordEndpoint, ivc_close, ivc_open,
        ivc_recv, ivc_send,
    };
    use ax_std::{
        format, println,
        string::{String, ToString},
        time::{Duration, Instant},
    };

    const ARCEOS_ACK_BODY: &[u8] = b"ack from arceos subscriber";
    const LINUX_ACK_BODY: &[u8] = b"ack from linux subscriber";
    const APP_HEADER_LEN: usize = 11;
    // Exercise single-fragment and full-ring payload boundaries.
    const APP_MAX_MESSAGE_LEN: usize = 7484;
    const REQUEST_MESSAGE_LENGTHS: [usize; 5] = [231, 232, 233, 7424, 7484];
    const SUBSCRIBER_DATA_MESSAGE_LENGTHS: [usize; 3] = [233, 7425, 7484];
    const PUBLISH_COUNT: u64 = REQUEST_MESSAGE_LENGTHS.len() as u64;
    const LOG_BODY_LIMIT: usize = 256;
    const SESSION_TIMEOUT: Duration = Duration::from_secs(30);
    const TRANSFER_IDLE_TIMEOUT: Duration = Duration::from_secs(30);
    const MESSAGE_SMOKE_MARKER: &str = "axvisor.ivc_case=arceos-message-smoke";
    const MESSAGE_SMOKE_TIMEOUT_MS: i32 = 30_000;
    const MESSAGE_SMOKE_REQUEST: [u8; 48] = smoke_payload(0x20);
    const MESSAGE_SMOKE_RESPONSE: [u8; 48] = smoke_payload(0x80);
    const MESSAGE_SMOKE_FINAL: [u8; 48] = smoke_payload(0xc0);

    pub fn run() {
        if message_smoke_enabled() {
            if let Err(error) = run_message_smoke() {
                println!("ivc message smoke failed backend=ivshmem-pci role=publisher: {error}");
            }
            return;
        }

        println!("ivc publisher backend=IvshmemPci");
        if let Err(error) = run_inner() {
            println!(
                "ivc session failed backend=ivshmem-pci role={}: {error}",
                IvcPeerRole::Publisher.as_str()
            );
        }
    }

    fn run_inner() -> Result<(), String> {
        let mut device = IvcDevice::open(IvcPeerRole::Publisher).map_err(stringify)?;
        let device_info = device.info();
        println!(
            "ivc publisher backend=ivshmem-pci peer={} max_peers={} event_mode=event-status-poll \
             msi_x=unavailable",
            device_info.peer_id(),
            device_info.max_peers()
        );

        let pending = device.open_endpoint().map_err(stringify)?;
        let pending_info = pending.info();
        println!(
            "ivc session marker=waiting backend=ivshmem-pci role=publisher peer={} \
             local_incarnation={} phase=before-peer",
            pending_info.device().peer_id(),
            pending_info.local_incarnation()
        );

        let mut endpoint = pending.wait_ready(SESSION_TIMEOUT).map_err(stringify)?;
        let ready = endpoint.info();
        println!(
            "ivc session marker=ready backend=ivshmem-pci role=publisher peer={} \
             local_incarnation={} peer_incarnation={}",
            ready.device().peer_id(),
            ready.local_incarnation(),
            ready.peer_incarnation()
        );
        let mut event_observed = ready.observed_event();
        if event_observed {
            println!("ivc session event backend=ivshmem-pci source=event-status");
        }

        let mut sender = RequestSender::new();
        let mut receiver = ResponseReceiver::new();
        let mut last_progress = Instant::now();

        while !sender.is_complete() || !receiver.is_complete() {
            let sent = sender.poll(&mut endpoint)?;
            let received = receiver.poll(&mut endpoint)?;
            if sent || received {
                last_progress = Instant::now();
            }
            if sender.is_complete() && receiver.is_complete() {
                break;
            }
            if last_progress.elapsed() >= TRANSFER_IDLE_TIMEOUT {
                return Err(format!(
                    "full-duplex exchange timed out sent={} received_acks={} received_data={}",
                    sender.sent_count(),
                    receiver.ack_count(),
                    receiver.data_count()
                ));
            }
            if !sent && !received {
                let observed = endpoint.wait_for_progress().map_err(stringify)?;
                if observed && !event_observed {
                    println!("ivc session event backend=ivshmem-pci source=event-status");
                }
                event_observed |= observed;
            }
        }

        println!(
            "ivc session marker=complete backend=ivshmem-pci role=publisher peer={}",
            ready.device().peer_id()
        );
        println!("ivc full-duplex demo complete backend=ivshmem-pci");
        Ok(())
    }

    fn run_message_smoke() -> Result<(), String> {
        let mut device = IvcDevice::discover().map_err(stringify)?;
        let info = device.info();
        println!(
            "ivc message smoke start backend=ivshmem-pci role={} peer={} max_peers={}",
            info.role().as_str(),
            info.peer_id(),
            info.max_peers()
        );

        let mut session = ivc_open(&mut device).map_err(stringify)?;
        ivc_send(
            &mut session,
            &MESSAGE_SMOKE_REQUEST,
            MESSAGE_SMOKE_TIMEOUT_MS,
        )
        .map_err(stringify)?;

        let mut response = [0u8; 48];
        let response_len =
            ivc_recv(&mut session, &mut response, MESSAGE_SMOKE_TIMEOUT_MS).map_err(stringify)?;
        if response_len != response.len() || response != MESSAGE_SMOKE_RESPONSE {
            return Err(format!(
                "response mismatch len={response_len} expected_len={}",
                MESSAGE_SMOKE_RESPONSE.len()
            ));
        }
        ivc_send(&mut session, &MESSAGE_SMOKE_FINAL, MESSAGE_SMOKE_TIMEOUT_MS)
            .map_err(stringify)?;

        let mut closed = [0u8; 48];
        match ivc_recv(&mut session, &mut closed, MESSAGE_SMOKE_TIMEOUT_MS) {
            Err(IvcError::SessionReset) => {}
            Ok(len) => return Err(format!("expected session close, received len={len}")),
            Err(error) => return Err(stringify(error)),
        }
        match ivc_recv(&mut session, &mut closed, 0) {
            Err(IvcError::SessionReset) => {}
            Ok(len) => {
                return Err(format!(
                    "session remained usable after close, recv len={len}"
                ));
            }
            Err(error) => return Err(stringify(error)),
        }
        ivc_close(session).map_err(stringify)?;
        let reopened = ivc_open(&mut device).map_err(stringify)?;
        ivc_close(reopened).map_err(stringify)?;

        println!(
            "ivc message smoke pass backend=ivshmem-pci role={} peer={} len={response_len} \
             path=48-byte",
            info.role().as_str(),
            info.peer_id()
        );
        Ok(())
    }

    struct RequestSender {
        payload: [u8; APP_MAX_MESSAGE_LEN],
        next_request: usize,
        active_len: usize,
        consumed: usize,
    }

    impl RequestSender {
        fn new() -> Self {
            Self {
                payload: [0; APP_MAX_MESSAGE_LEN],
                next_request: 0,
                active_len: 0,
                consumed: 0,
            }
        }

        fn poll(&mut self, endpoint: &mut IvcEndpoint<'_>) -> Result<bool, String> {
            if self.is_complete() {
                return Ok(false);
            }
            if self.active_len == 0 {
                self.start_next(endpoint)?;
            }

            let mut wrote = false;
            loop {
                let progress = endpoint
                    .write_record(&self.payload[self.consumed..self.active_len])
                    .map_err(stringify)?;
                self.consumed += progress.transferred();
                wrote |= progress.progressed();
                if progress.is_complete() {
                    let sequence = self.next_request as u64 + 1;
                    println!("ivc send seq={sequence} len={}", self.active_len);
                    self.next_request += 1;
                    self.active_len = 0;
                    self.consumed = 0;
                    return Ok(true);
                }
                if !progress.progressed() {
                    return Ok(wrote);
                }
            }
        }

        fn start_next(&mut self, endpoint: &mut IvcEndpoint<'_>) -> Result<(), String> {
            let sequence = self.next_request as u64 + 1;
            let message_len = REQUEST_MESSAGE_LENGTHS[self.next_request];
            if !encode_pattern_message(
                &mut self.payload[..message_len],
                AppMessageKind::Request,
                sequence,
            ) {
                return Err(format!("cannot encode request seq={sequence}"));
            }
            endpoint
                .start_record(message_len as u64)
                .map_err(stringify)?;
            self.active_len = message_len;
            self.consumed = 0;
            Ok(())
        }

        const fn is_complete(&self) -> bool {
            self.next_request == REQUEST_MESSAGE_LENGTHS.len()
        }

        const fn sent_count(&self) -> usize {
            self.next_request
        }
    }

    struct ResponseReceiver {
        payload: [u8; APP_MAX_MESSAGE_LEN],
        received: usize,
        expected_ack_sequence: u64,
        expected_data_sequence: u64,
        complete: bool,
    }

    impl ResponseReceiver {
        fn new() -> Self {
            Self {
                payload: [0; APP_MAX_MESSAGE_LEN],
                received: 0,
                expected_ack_sequence: 1,
                expected_data_sequence: 1,
                complete: false,
            }
        }

        fn poll(&mut self, endpoint: &mut IvcEndpoint<'_>) -> Result<bool, String> {
            if self.complete {
                return Ok(false);
            }
            let mut progressed = false;
            loop {
                if self.received == 0 {
                    match endpoint.peek_record_len().map_err(stringify)? {
                        Some(message_len) if message_fits(message_len, self.payload.len()) => {}
                        Some(message_len) => {
                            return Err(format!("oversized reply message len={message_len}"));
                        }
                        None => return Ok(progressed),
                    }
                }

                let progress = endpoint
                    .read_record(&mut self.payload[self.received..])
                    .map_err(stringify)?;
                self.received += progress.transferred();
                progressed |= progress.progressed();
                if progress.is_complete() {
                    self.validate_message()?;
                    self.received = 0;
                    if self.complete {
                        return Ok(true);
                    }
                } else if !progress.progressed() {
                    return Ok(progressed);
                }
            }
        }

        fn validate_message(&mut self) -> Result<(), String> {
            let message = decode_app_message(&self.payload[..self.received])
                .ok_or("malformed application reply payload")?;
            match message.kind {
                AppMessageKind::Ack => {
                    if message.sequence != self.expected_ack_sequence
                        || (message.body != ARCEOS_ACK_BODY && message.body != LINUX_ACK_BODY)
                    {
                        return Err(format!(
                            "ack expected={} actual={} len={}",
                            self.expected_ack_sequence,
                            message.sequence,
                            message.body.len()
                        ));
                    }
                    if self.expected_ack_sequence == PUBLISH_COUNT
                        && self.expected_data_sequence
                            != SUBSCRIBER_DATA_MESSAGE_LENGTHS.len() as u64 + 1
                    {
                        return Err(format!(
                            "missing subscriber data expected={}",
                            self.expected_data_sequence
                        ));
                    }
                    let text = core::str::from_utf8(message.body).unwrap_or("<non-utf8>");
                    println!("ivc ack seq={} msg={text}", message.sequence);
                    self.expected_ack_sequence += 1;
                    if self.expected_ack_sequence == PUBLISH_COUNT + 1 {
                        self.complete = true;
                    }
                }
                AppMessageKind::Data => {
                    let expected_len = *SUBSCRIBER_DATA_MESSAGE_LENGTHS
                        .get((self.expected_data_sequence - 1) as usize)
                        .ok_or_else(|| {
                            format!("unexpected subscriber data seq={}", message.sequence)
                        })?;
                    if !validate_pattern_message(
                        &message,
                        AppMessageKind::Data,
                        self.expected_data_sequence,
                        expected_len,
                    ) {
                        return Err(format!(
                            "subscriber data expected={} actual={} len={}",
                            self.expected_data_sequence, message.sequence, self.received
                        ));
                    }
                    let text = body_for_serial_log(message.body);
                    println!(
                        "ivc recv data seq={} len={} msg={text}",
                        message.sequence, self.received
                    );
                    self.expected_data_sequence += 1;
                }
                AppMessageKind::Request => {
                    return Err("unexpected Request on reply ring".into());
                }
            }
            Ok(())
        }

        const fn is_complete(&self) -> bool {
            self.complete
        }

        const fn ack_count(&self) -> u64 {
            self.expected_ack_sequence - 1
        }

        const fn data_count(&self) -> u64 {
            self.expected_data_sequence - 1
        }
    }

    fn encode_pattern_message(payload: &mut [u8], kind: AppMessageKind, sequence: u64) -> bool {
        let Some(body_len) = payload.len().checked_sub(APP_HEADER_LEN) else {
            return false;
        };
        let Ok(body_len) = u16::try_from(body_len) else {
            return false;
        };
        payload[0] = kind as u8;
        payload[1..9].copy_from_slice(&sequence.to_le_bytes());
        payload[9..APP_HEADER_LEN].copy_from_slice(&body_len.to_le_bytes());
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

    fn body_for_serial_log(body: &[u8]) -> &str {
        if body.len() > LOG_BODY_LIMIT {
            "<omitted>"
        } else {
            core::str::from_utf8(body).unwrap_or("<non-utf8>")
        }
    }

    const fn smoke_payload(seed: u8) -> [u8; 48] {
        let mut payload = [0u8; 48];
        let mut index = 0;
        while index < payload.len() {
            payload[index] = seed.wrapping_add((index as u8).wrapping_mul(3));
            index += 1;
        }
        payload
    }

    fn message_smoke_enabled() -> bool {
        ax_std::os::arceos::modules::ax_hal::boot::bootargs()
            .is_some_and(|bootargs| bootargs.contains(MESSAGE_SMOKE_MARKER))
    }

    fn stringify(error: impl ToString) -> String {
        error.to_string()
    }
}
