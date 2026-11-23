#![cfg_attr(feature = "arceos", no_main)]
#![cfg_attr(feature = "arceos", no_std)]

#[cfg(feature = "arceos")]
use ax_std as _;

#[cfg_attr(feature = "arceos", unsafe(no_mangle))]
#[cfg(feature = "arceos")]
fn main() {
    publisher::run();
}

#[cfg(feature = "arceos")]
mod demo_config {
    pub const CHANNEL_KEY: usize = 0x4956_4301;
    pub const CHANNEL_SIZE: usize = 0x1_0000;
    pub const NOTIFY_IRQ: Option<usize> = Some(160);
    pub const PUBLISHER_VM_ID: usize = 1;
    pub const SUBSCRIBER_VM_ID: usize = 2;
}

#[cfg(not(feature = "arceos"))]
fn main() {}

#[cfg(feature = "arceos")]
mod publisher {
    use core::{cell::UnsafeCell, result::Result::Err, sync::atomic::AtomicU64};

    use arceos_ivshmem_pci::{EVENT_VECTOR_0, IvshmemDevice, IvshmemLayout};
    use ax_std::{
        format,
        os::arceos::modules::ax_hal::{
            self, irq,
            mem::{PhysAddr, VirtAddr, virt_to_phys},
        },
        println,
        string::{String, ToString},
        thread,
        time::{Duration, Instant},
    };
    use axhvc::ivc::{self, IvcGuestPhysAddr};
    use axivc::{
        IvcMessageReceiver, IvcMessageSender, IvcPeerEventWaiter, IvcRegion, IvshmemMessagePort,
        IvshmemSessionState, record_peer_event,
    };

    const ARCEOS_ACK_BODY: &[u8] = b"ack from arceos subscriber";
    const LINUX_ACK_BODY: &[u8] = b"ack from linux subscriber";
    const APP_HEADER_LEN: usize = 11;
    // Exercise single-fragment and full-ring payload boundaries.
    const APP_MAX_MESSAGE_LEN: usize = 7484;
    const REQUEST_MESSAGE_LENGTHS: [usize; 5] = [231, 232, 233, 7424, 7484];
    const SUBSCRIBER_DATA_MESSAGE_LENGTHS: [usize; 3] = [233, 7425, 7484];
    const PUBLISH_COUNT: u64 = REQUEST_MESSAGE_LENGTHS.len() as u64;
    const IVSHMEM_PEER_ID: u32 = 0;
    const IVSHMEM_PEER_SLOT: u32 = 1;
    const IVSHMEM_BACKEND_ARG: &str = "axvisor.ivc_backend=";
    const SESSION_TIMEOUT: Duration = Duration::from_secs(30);
    const TRANSFER_TIMEOUT: Duration = Duration::from_secs(30);
    const EVENT_POLL_DELAY: Duration = Duration::from_millis(1);
    static NOTIFY_IRQ_COUNT: AtomicU64 = AtomicU64::new(0);

    use crate::demo_config;

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum PublisherBackend {
        IvshmemPci,
        Legacy,
    }

    pub fn run() {
        let backend = match requested_backend() {
            Ok(backend) => backend,
            Err(error) => {
                println!("ivc session failed backend selection: {error}");
                return;
            }
        };
        println!("ivc publisher backend={backend:?}");
        match backend {
            PublisherBackend::IvshmemPci => ivshmem_run(),
            PublisherBackend::Legacy => legacy_run(),
        }
    }

    fn requested_backend() -> Result<PublisherBackend, &'static str> {
        let bootargs = ax_hal::boot::bootargs().unwrap_or("");
        let mut requested = None;
        for argument in bootargs.split_whitespace() {
            let Some(value) = argument.strip_prefix(IVSHMEM_BACKEND_ARG) else {
                continue;
            };
            if requested.replace(value).is_some() {
                return Err("multiple axvisor.ivc_backend markers");
            }
        }
        match requested {
            None | Some("ivshmem-pci") => Ok(PublisherBackend::IvshmemPci),
            Some("legacy") => Ok(PublisherBackend::Legacy),
            Some(_) => Err("unsupported axvisor.ivc_backend value"),
        }
    }

    fn ivshmem_run() {
        if let Err(error) = ivshmem_run_inner() {
            println!("ivc session failed backend=ivshmem-pci peer={IVSHMEM_PEER_ID}: {error}");
        }
    }

    fn ivshmem_run_inner() -> Result<(), String> {
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
            "ivc publisher backend=ivshmem-pci peer={} max_peers={} event_mode=event-status-poll \
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
            "ivc session marker=waiting backend=ivshmem-pci role=publisher peer={} \
             local_incarnation={local_incarnation} phase=before-peer",
            device.peer_id()
        );

        wait_for_session(&device, &mut port, local_incarnation)?;
        let (sender, receiver) = port.try_connect().map_err(|e| e.to_string())?.into_parts();
        let mut sender = RequestSender::new(sender);
        let mut receiver = ResponseReceiver::new(receiver);
        let started = Instant::now();
        let mut event_observed = false;

        while !sender.is_complete() || !receiver.is_complete() {
            let sent = sender.poll()?;
            let received = receiver.poll()?;
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
                    "full-duplex exchange timed out sent={} received_acks={} received_data={}",
                    sender.sent_count(),
                    receiver.ack_count(),
                    receiver.data_count()
                ));
            }
            if !sent && !received {
                thread::sleep(EVENT_POLL_DELAY);
            }
        }

        println!(
            "ivc session marker=complete backend=ivshmem-pci role=publisher peer={}",
            device.peer_id()
        );
        println!("ivc full-duplex demo complete backend=ivshmem-pci");
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
                        "ivc session marker=ready backend=ivshmem-pci role=publisher peer={} \
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

    struct RequestSender<'a> {
        sender: IvcMessageSender<'a>,
        payload: [u8; APP_MAX_MESSAGE_LEN],
        next_request: usize,
        active_len: usize,
        consumed: usize,
    }

    impl<'a> RequestSender<'a> {
        fn new(sender: IvcMessageSender<'a>) -> Self {
            Self {
                sender,
                payload: [0; APP_MAX_MESSAGE_LEN],
                next_request: 0,
                active_len: 0,
                consumed: 0,
            }
        }

        fn poll(&mut self) -> Result<bool, String> {
            if self.is_complete() {
                return Ok(false);
            }
            if self.active_len == 0 {
                self.start_next()?;
            }

            let mut wrote = false;
            loop {
                let progress = self
                    .sender
                    .try_write(&self.payload[self.consumed..self.active_len])
                    .map_err(|e| format!("{e:?}"))?;
                self.consumed += progress.consumed();
                wrote |= progress.published_slots() > 0;
                if progress.is_complete() {
                    let sequence = self.next_request as u64;
                    println!("ivc send seq={sequence} len={}", self.active_len);
                    self.next_request += 1;
                    self.active_len = 0;
                    self.consumed = 0;
                    return Ok(true);
                }
                if progress.consumed() == 0 {
                    return Ok(wrote);
                }
            }
        }

        fn start_next(&mut self) -> Result<(), String> {
            let sequence = self.next_request as u64 + 1;
            let message_len = REQUEST_MESSAGE_LENGTHS[self.next_request];
            if !encode_pattern_message(
                &mut self.payload[..message_len],
                AppMessageKind::Request,
                sequence,
            ) {
                return Err(format!("cannot encode request seq={sequence}"));
            }
            self.sender
                .start_message(message_len as u64)
                .map_err(|e| format!("{e:?}"))?;
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

    struct ResponseReceiver<'a> {
        receiver: IvcMessageReceiver<'a>,
        payload: [u8; APP_MAX_MESSAGE_LEN],
        received: usize,
        expected_ack_sequence: u64,
        expected_data_sequence: u64,
        complete: bool,
    }

    impl<'a> ResponseReceiver<'a> {
        fn new(receiver: IvcMessageReceiver<'a>) -> Self {
            Self {
                receiver,
                payload: [0; APP_MAX_MESSAGE_LEN],
                received: 0,
                expected_ack_sequence: 1,
                expected_data_sequence: 1,
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
                            return Err(format!("oversized reply message len={}", meta.len()));
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
                    self.validate_message()?;
                    self.received = 0;
                    if self.complete {
                        return Ok(true);
                    }
                } else if progress.consumed_slots() == 0 {
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
                    let text = core::str::from_utf8(message.body).unwrap_or("<non-utf8>");
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

    fn legacy_run() {
        let shm_base_gpa = HyperCallOutputValue::new(0);
        let shm_size = HyperCallOutputValue::new(demo_config::CHANNEL_SIZE);
        let shm_base_gpa_ptr = shm_base_gpa.guest_phys_addr();
        let shm_size_ptr = shm_size.guest_phys_addr();

        if let Err(err) =
            ivc::publish_channel(demo_config::CHANNEL_KEY, shm_base_gpa_ptr, shm_size_ptr)
        {
            println!("ivc publish failed: {err}");
            return;
        }
        let shm_base_gpa = shm_base_gpa.read();
        let shm_size = shm_size.read();
        if shm_size < core::mem::size_of::<IvcRegion>() {
            println!(
                "ivc publish failed: shared page too small size={} need={}",
                shm_size,
                core::mem::size_of::<IvcRegion>()
            );
            return;
        }

        println!("ivc publish ok base={shm_base_gpa:#x} size={shm_size}");
        let Some(region) = shared_region(shm_base_gpa, shm_size) else {
            println!("ivc publish failed: map shared page base={shm_base_gpa:#x}");
            return;
        };
        // SAFETY: publish_channel created fresh backing whose full range was
        // zeroed and whose host header was installed before publication. This
        // demo publisher is the unique initializer and has created no endpoint.
        // A subscriber may poll the atomic protocol header, but attaches only
        // after the final magic is published by this call.
        unsafe { region.initialize() };
        // SAFETY: initialization completed above. This app creates the unique
        // publisher role once, keeps the mapping valid until both tasks finish,
        // and never resets the region during this run.
        let (publish_producer, publish_consumer) =
            unsafe { region.publisher_endpoints() }.into_parts();
        let irq_enabled = register_notify_irq();

        // Each task owns its waiter: `IvcPeerEventWaiter` tracks the observed
        // event count internally, so sharing one waiter would let one task
        // consume IRQ observations meant to wake the other.
        let sender = thread::spawn(move || {
            let waiter = IvcPeerEventWaiter::new(irq_enabled, &NOTIFY_IRQ_COUNT);
            sender_task(publish_producer, &waiter);
        });
        let receiver = thread::spawn(move || {
            let waiter = IvcPeerEventWaiter::new(irq_enabled, &NOTIFY_IRQ_COUNT);
            receiver_task(publish_consumer, &waiter);
        });

        sender.join().expect("sender thread panicked");
        receiver.join().expect("receiver thread panicked");

        println!("ivc full-duplex demo complete");
    }

    /// Sends messages spanning one-slot, fragment-boundary, ring-boundary, and
    /// backpressured lengths without waiting for individual acknowledgements.
    fn sender_task(mut sender: IvcMessageSender<'_>, waiter: &IvcPeerEventWaiter<'_>) {
        let mut payload = [0u8; APP_MAX_MESSAGE_LEN];
        let mut subscriber_ready = false;
        for (index, &message_len) in REQUEST_MESSAGE_LENGTHS.iter().enumerate() {
            let sequence = index as u64 + 1;
            let message = &mut payload[..message_len];
            if !encode_pattern_message(message, AppMessageKind::Request, sequence) {
                println!("ivc validation failed: cannot encode request seq={sequence}");
                return;
            }
            if !send_payload(&mut sender, message, waiter, &mut subscriber_ready) {
                return;
            }
            println!("ivc send seq={sequence} len={message_len}");
        }
    }

    /// Receives independently sequenced Data and Ack messages and rejects any
    /// duplicate, gap, reorder, length mismatch, or body corruption.
    fn receiver_task(mut receiver: IvcMessageReceiver<'_>, waiter: &IvcPeerEventWaiter<'_>) {
        let mut payload = [0u8; APP_MAX_MESSAGE_LEN];
        let mut received = 0;
        let mut expected_ack_sequence = 1u64;
        let mut expected_data_sequence = 1u64;
        loop {
            if received == 0 {
                match receiver.peek_message_meta() {
                    Ok(Some(meta)) if message_fits(meta.len(), payload.len()) => {}
                    Ok(Some(meta)) => {
                        println!("ivc recv error oversized message len={}", meta.len());
                        return;
                    }
                    Ok(None) => {
                        waiter.wait_for_peer_event();
                        continue;
                    }
                    Err(err) => {
                        println!("ivc recv error {err:?}");
                        return;
                    }
                }
            }

            match receiver.try_read(&mut payload[received..]) {
                Ok(progress) => {
                    received += progress.written();
                    if progress.consumed_slots() > 0 {
                        notify_subscriber();
                    }
                    if progress.is_complete() {
                        let Some(message) = decode_app_message(&payload[..received]) else {
                            println!("ivc recv error malformed application payload");
                            return;
                        };
                        match message.kind {
                            AppMessageKind::Ack => {
                                if message.sequence != expected_ack_sequence
                                    || (message.body != ARCEOS_ACK_BODY
                                        && message.body != LINUX_ACK_BODY)
                                {
                                    println!(
                                        "ivc validation failed: ack expected={} actual={} len={}",
                                        expected_ack_sequence,
                                        message.sequence,
                                        message.body.len()
                                    );
                                    return;
                                }
                                if expected_ack_sequence == PUBLISH_COUNT
                                    && expected_data_sequence
                                        != SUBSCRIBER_DATA_MESSAGE_LENGTHS.len() as u64 + 1
                                {
                                    println!(
                                        "ivc validation failed: missing subscriber data \
                                         expected={}",
                                        expected_data_sequence
                                    );
                                    return;
                                }
                                let text =
                                    core::str::from_utf8(message.body).unwrap_or("<non-utf8>");
                                println!("ivc ack seq={} msg={text}", message.sequence);
                                expected_ack_sequence += 1;
                                if expected_ack_sequence == PUBLISH_COUNT + 1 {
                                    return;
                                }
                            }
                            AppMessageKind::Data => {
                                let Some(&expected_len) = SUBSCRIBER_DATA_MESSAGE_LENGTHS
                                    .get((expected_data_sequence - 1) as usize)
                                else {
                                    println!(
                                        "ivc validation failed: unexpected subscriber data seq={}",
                                        message.sequence
                                    );
                                    return;
                                };
                                if !validate_pattern_message(
                                    &message,
                                    AppMessageKind::Data,
                                    expected_data_sequence,
                                    expected_len,
                                ) {
                                    println!(
                                        "ivc validation failed: subscriber data expected={} \
                                         actual={} len={}",
                                        expected_data_sequence, message.sequence, received
                                    );
                                    return;
                                }
                                let text =
                                    core::str::from_utf8(message.body).unwrap_or("<non-utf8>");
                                println!(
                                    "ivc recv data seq={} len={received} msg={text}",
                                    message.sequence
                                );
                                expected_data_sequence += 1;
                            }
                            AppMessageKind::Request => {
                                println!("ivc validation failed: unexpected Request on reply ring");
                                return;
                            }
                        }
                        received = 0;
                    } else if progress.consumed_slots() == 0 {
                        waiter.wait_for_peer_event();
                    }
                }
                Err(err) => {
                    println!("ivc recv ack error {err:?}");
                    return;
                }
            }
        }
    }

    fn send_payload(
        sender: &mut IvcMessageSender<'_>,
        payload: &[u8],
        waiter: &IvcPeerEventWaiter<'_>,
        peer_ready: &mut bool,
    ) -> bool {
        if let Err(err) = sender.start_message(payload.len() as u64) {
            println!("ivc send error {err:?}");
            return false;
        }

        let mut consumed = 0;
        loop {
            match sender.try_write(&payload[consumed..]) {
                Ok(progress) => {
                    consumed += progress.consumed();
                    if progress.published_slots() > 0 {
                        if *peer_ready {
                            notify_subscriber();
                        }
                        *peer_ready = true;
                    }
                    if progress.is_complete() {
                        return true;
                    }
                    if progress.published_slots() == 0 {
                        waiter.wait_for_peer_event();
                    }
                }
                Err(err) => {
                    println!("ivc send error {err:?}");
                    return false;
                }
            }
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

    fn notify_subscriber() {
        if let Err(err) = ivc::notify_channel(
            demo_config::PUBLISHER_VM_ID,
            demo_config::CHANNEL_KEY,
            demo_config::SUBSCRIBER_VM_ID,
        ) {
            println!("ivc notify warning: {err}");
        }
    }

    fn register_notify_irq() -> bool {
        let Some(raw_irq) = demo_config::NOTIFY_IRQ else {
            return false;
        };
        match notify_irq_id(raw_irq)
            .and_then(|irq_id| irq::request_shared_irq(irq_id, notify_irq_handler).map(|_| irq_id))
        {
            Ok(irq_id) => {
                println!("ivc notify irq enabled irq={irq_id:?}");
                true
            }
            Err(err) => {
                println!("ivc notify irq disabled raw={raw_irq} err={err:?}");
                false
            }
        }
    }

    fn notify_irq_handler(_ctx: irq::IrqContext) -> irq::IrqReturn {
        record_peer_event(&NOTIFY_IRQ_COUNT);
        irq::IrqReturn::Handled
    }

    fn notify_irq_id(raw_irq: usize) -> Result<irq::IrqId, irq::IrqError> {
        #[cfg(target_arch = "aarch64")]
        {
            let gsi = u32::try_from(raw_irq).map_err(|_| irq::IrqError::InvalidIrq)?;
            irq::resolve_irq_source(irq::IrqSource::AcpiGsi(gsi))
        }
        #[cfg(not(target_arch = "aarch64"))]
        {
            irq::try_legacy_irq(raw_irq)
        }
    }

    struct HyperCallOutputValue {
        value: UnsafeCell<usize>,
    }

    impl HyperCallOutputValue {
        const fn new(value: usize) -> Self {
            Self {
                value: UnsafeCell::new(value),
            }
        }

        fn guest_phys_addr(&self) -> IvcGuestPhysAddr {
            let vaddr = VirtAddr::from_usize(self.value.get().addr());
            IvcGuestPhysAddr::new(virt_to_phys(vaddr).as_usize())
        }

        fn read(&self) -> usize {
            unsafe {
                // Axvisor writes this value through the guest physical address
                // passed to the hypercall; use a volatile read to observe it.
                core::ptr::read_volatile(self.value.get())
            }
        }
    }

    fn shared_region(shm_base_gpa: usize, shm_size: usize) -> Option<&'static IvcRegion> {
        let vaddr = ax_mm::iomap(PhysAddr::from_usize(shm_base_gpa), shm_size).ok()?;
        // SAFETY: the HVC mapping is valid and page-aligned, and `run` checked
        // that its size covers `IvcRegion`. Axvisor zeroed the entire fresh
        // backing before writing the publisher_id/key host header, so every
        // field has a valid initial bit pattern. The host header is not changed
        // after publication, and the mapping remains valid for this run. Shared
        // access is required so peers may poll atomic protocol metadata.
        unsafe { Some(&*(vaddr.as_ptr() as *const IvcRegion)) }
    }
}
