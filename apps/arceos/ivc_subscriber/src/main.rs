#![cfg_attr(feature = "arceos", no_main)]
#![cfg_attr(feature = "arceos", no_std)]

#[cfg(feature = "arceos")]
use ax_std as _;

#[cfg_attr(feature = "arceos", unsafe(no_mangle))]
#[cfg(feature = "arceos")]
fn main() {
    subscriber::run();
}

#[cfg(any(feature = "arceos", test))]
mod demo_config {
    pub const CHANNEL_KEY: usize = 0x4956_4301;
    pub const NOTIFY_IRQ: Option<usize> = Some(160);
    pub const PUBLISHER_VM_ID: usize = 1;
    pub const SUBSCRIBER_VM_ID: usize = 2;
}

#[cfg(not(feature = "arceos"))]
fn main() {}

#[cfg(feature = "arceos")]
mod subscriber {
    use core::{
        cell::UnsafeCell,
        option::Option::{None, Some},
        result::Result::{Err, Ok},
        sync::atomic::{AtomicU64, Ordering},
    };

    use ax_std::{
        os::arceos::modules::ax_hal::{
            irq,
            mem::{PhysAddr, VirtAddr, virt_to_phys},
        },
        println, thread,
    };
    use axhvc::ivc::{self, IvcGuestPhysAddr};
    use axivc::{
        IvcMessageReceiver, IvcMessageSender, IvcPeerEventWaiter, IvcRegion, record_peer_event,
    };

    const ACK_BODY: &[u8] = b"ack from arceos subscriber";
    const APP_HEADER_LEN: usize = 11;
    const APP_MAX_MESSAGE_LEN: usize = 700;
    const ACK_MESSAGE_LEN: usize = APP_HEADER_LEN + ACK_BODY.len();
    const DATA_MESSAGE_LENGTHS: [usize; 3] = [41, 641, 700];
    const REQUEST_MESSAGE_LENGTHS: [usize; 5] = [39, 40, 41, 640, 700];
    const MAX_SUBSCRIBE_ATTEMPTS: usize = 80;
    const PUBLISH_COUNT: u64 = REQUEST_MESSAGE_LENGTHS.len() as u64;
    const SUBSCRIBE_DATA_COUNT: u64 = DATA_MESSAGE_LENGTHS.len() as u64;
    static NOTIFY_IRQ_COUNT: AtomicU64 = AtomicU64::new(0);
    /// Highest publisher sequence received so far. Publisher sequences are
    /// contiguous, so one monotonic counter covers every pending ack.
    static HIGHEST_RECV_SEQ: AtomicU64 = AtomicU64::new(0);

    use crate::demo_config;

    pub fn run() {
        let irq_enabled = register_notify_irq();
        let waiter = IvcPeerEventWaiter::new(irq_enabled, &NOTIFY_IRQ_COUNT);
        let Some((shm_base_gpa, shm_size)) = subscribe_with_retry(&waiter) else {
            println!("ivc subscribe failed: retry limit reached");
            return;
        };

        println!(
            "ivc subscribe ok subscriber={} base={shm_base_gpa:#x} size={shm_size}",
            demo_config::SUBSCRIBER_VM_ID
        );
        if shm_size < core::mem::size_of::<IvcRegion>() {
            println!(
                "ivc subscribe failed: shared page too small size={} need={}",
                shm_size,
                core::mem::size_of::<IvcRegion>()
            );
            return;
        }

        let Some(region) = shared_region(shm_base_gpa, shm_size) else {
            println!("ivc subscribe failed: map shared page base={shm_base_gpa:#x}");
            return;
        };
        if !region.channel_header_matches(demo_config::PUBLISHER_VM_ID, demo_config::CHANNEL_KEY) {
            println!(
                "ivc subscribe failed: unexpected header publisher/key for base={shm_base_gpa:#x}"
            );
            return;
        }
        if !wait_for_protocol_header(region, &waiter) {
            println!("ivc subscribe failed: unsupported Message V1 protocol header");
            return;
        }

        let region: &'static IvcRegion = region;
        // SAFETY: this app is the only subscriber side of this channel and
        // creates each endpoint exactly once; the producer moves into the
        // sender task and the consumer into the receiver task.
        let (reply_producer, request_consumer) =
            unsafe { region.subscriber_endpoints() }.into_parts();

        // Each task owns its waiter: `IvcPeerEventWaiter` tracks the observed
        // event count internally, so sharing one waiter would let one task
        // consume IRQ observations meant to wake the other.
        let sender = thread::spawn(move || {
            let waiter = IvcPeerEventWaiter::new(irq_enabled, &NOTIFY_IRQ_COUNT);
            sender_task(reply_producer, &waiter);
        });
        let receiver = thread::spawn(move || {
            let waiter = IvcPeerEventWaiter::new(irq_enabled, &NOTIFY_IRQ_COUNT);
            receiver_task(request_consumer, &waiter);
        });

        sender.join().expect("sender thread panicked");
        receiver.join().expect("receiver thread panicked");

        println!("ivc subscriber full-duplex demo complete");
    }

    fn wait_for_protocol_header(region: &IvcRegion, waiter: &IvcPeerEventWaiter<'_>) -> bool {
        for _ in 0..MAX_SUBSCRIBE_ATTEMPTS {
            if region.protocol_header_matches() {
                return true;
            }
            waiter.wait_for_peer_event();
        }
        false
    }

    fn subscribe_with_retry(waiter: &IvcPeerEventWaiter<'_>) -> Option<(usize, usize)> {
        for attempt in 1..=MAX_SUBSCRIBE_ATTEMPTS {
            let shm_base_gpa = HyperCallOutputValue::new(0);
            let shm_size = HyperCallOutputValue::new(0);
            let shm_base_gpa_ptr = shm_base_gpa.guest_phys_addr();
            let shm_size_ptr = shm_size.guest_phys_addr();

            match ivc::subscribe_channel(
                demo_config::PUBLISHER_VM_ID,
                demo_config::CHANNEL_KEY,
                shm_base_gpa_ptr,
                shm_size_ptr,
            ) {
                Ok(()) => return Some((shm_base_gpa.read(), shm_size.read())),
                Err(err) => {
                    if attempt == 1 || attempt % 10 == 0 {
                        println!("ivc subscribe retry attempt={attempt} err={err}");
                    }
                    waiter.wait_for_peer_event();
                }
            }
        }
        None
    }

    /// Sends independently sequenced Data and Ack messages on one Message V1
    /// direction without interleaving their fragments.
    fn sender_task(mut sender: IvcMessageSender<'_>, waiter: &IvcPeerEventWaiter<'_>) {
        let mut data_payload = [0u8; APP_MAX_MESSAGE_LEN];
        let mut sent_data = 0u64;
        let mut last_acked_sequence = 0u64;
        let mut publisher_ready = false;

        loop {
            if sent_data < SUBSCRIBE_DATA_COUNT {
                let sequence = sent_data + 1;
                let message_len = DATA_MESSAGE_LENGTHS[sent_data as usize];
                let payload = &mut data_payload[..message_len];
                if !encode_pattern_message(payload, AppMessageKind::Data, sequence) {
                    println!("ivc validation failed: cannot encode data seq={sequence}");
                    return;
                }
                if !send_payload(&mut sender, payload, waiter, &mut publisher_ready) {
                    return;
                }
                sent_data = sequence;
                println!("ivc send data seq={sent_data} len={message_len}");
            }

            let highest = HIGHEST_RECV_SEQ.load(Ordering::Acquire);
            while last_acked_sequence < highest {
                let sequence = last_acked_sequence + 1;
                let payload = encode_ack(sequence);
                if !send_payload(&mut sender, &payload, waiter, &mut publisher_ready) {
                    return;
                }
                last_acked_sequence = sequence;
                println!("ivc ack pub seq={last_acked_sequence}");
            }

            if sent_data == SUBSCRIBE_DATA_COUNT && last_acked_sequence == PUBLISH_COUNT {
                return;
            }
            waiter.wait_for_peer_event();
        }
    }

    /// Receives publisher Requests and rejects any duplicate, gap, reorder,
    /// length mismatch, kind mismatch, or body corruption.
    fn receiver_task(mut receiver: IvcMessageReceiver<'_>, waiter: &IvcPeerEventWaiter<'_>) {
        let mut payload = [0u8; APP_MAX_MESSAGE_LEN];
        let mut received = 0;
        let mut expected_sequence = 1u64;
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
                    if progress.consumed_cells() > 0 {
                        notify_publisher();
                    }
                    if progress.is_complete() {
                        let Some(message) = decode_app_message(&payload[..received]) else {
                            println!("ivc recv error malformed application payload");
                            return;
                        };
                        let Some(&expected_len) =
                            REQUEST_MESSAGE_LENGTHS.get((expected_sequence - 1) as usize)
                        else {
                            println!(
                                "ivc validation failed: unexpected request seq={}",
                                message.sequence
                            );
                            return;
                        };
                        if !validate_pattern_message(
                            &message,
                            AppMessageKind::Request,
                            expected_sequence,
                            expected_len,
                        ) {
                            println!(
                                "ivc validation failed: request expected={} actual={} len={}",
                                expected_sequence, message.sequence, received
                            );
                            return;
                        }
                        let text = core::str::from_utf8(message.body).unwrap_or("<non-utf8>");
                        println!(
                            "ivc recv request seq={} len={received} msg={text}",
                            message.sequence
                        );
                        HIGHEST_RECV_SEQ.store(expected_sequence, Ordering::Release);
                        expected_sequence += 1;
                        if expected_sequence == PUBLISH_COUNT + 1 {
                            return;
                        }
                        received = 0;
                    } else if progress.consumed_cells() == 0 {
                        waiter.wait_for_peer_event();
                    }
                }
                Err(err) => {
                    println!("ivc recv error {err:?}");
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
                    if progress.published_cells() > 0 {
                        if *peer_ready {
                            notify_publisher();
                        }
                        *peer_ready = true;
                    }
                    if progress.is_complete() {
                        return true;
                    }
                    if progress.published_cells() == 0 {
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

    fn notify_publisher() {
        if let Err(err) = ivc::notify_channel(
            demo_config::PUBLISHER_VM_ID,
            demo_config::CHANNEL_KEY,
            demo_config::PUBLISHER_VM_ID,
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
        // The hypervisor backs the shared region with host RAM and maps it at
        // stage 2 with normal-memory attributes, and the Linux peer maps the
        // same GPA through `memremap(MEMREMAP_WB)`. Message V1 ring indices
        // rely on acquire/release ordering, so both peers must map the region
        // with consistent normal cacheable attributes instead of Device ones.
        let vaddr = ax_mm::map_normal_memory(PhysAddr::from_usize(shm_base_gpa), shm_size).ok()?;
        unsafe {
            // Axvisor maps the returned GPA to the publisher's shared region.
            // Phase 2 uses atomic ring ownership for subscriber writes.
            Some(&*(vaddr.as_ptr() as *const IvcRegion))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qemu_ivc_notify_irq_matches_guest_config() {
        assert_eq!(demo_config::CHANNEL_KEY, 0x4956_4301);
        assert_eq!(demo_config::NOTIFY_IRQ, Some(160));
        assert_eq!(demo_config::PUBLISHER_VM_ID, 1);
        assert_eq!(demo_config::SUBSCRIBER_VM_ID, 2);
    }
}
