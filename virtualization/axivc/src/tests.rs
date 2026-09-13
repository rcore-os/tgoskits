use core::sync::atomic::AtomicU64;

use crate::{
    IVC_RING_CAPACITY, IVC_SLOT_PAYLOAD_SIZE, IvcMessageKind, IvcPeerEventWaiter, IvcRegion,
    IvcRingError, record_peer_event, region::new_region_for_test,
};

const CHANNEL_KEY: usize = 0x4956_4301;
const PUBLISHER_VM_ID: usize = 1;
const CHANNEL_SIZE: usize = 4096;

#[test]
fn region_header_and_channel_header_match_after_initialize() {
    let mut region = new_region(PUBLISHER_VM_ID, CHANNEL_KEY);

    region.initialize();

    assert!(region.channel_header_matches(PUBLISHER_VM_ID, CHANNEL_KEY));
    assert!(region.protocol_header_matches());
}

#[test]
fn request_ring_delivers_messages_in_fifo_order() {
    let mut region = new_region(PUBLISHER_VM_ID, CHANNEL_KEY);
    region.initialize();
    // SAFETY: this test attaches each channel role exactly once.
    let (mut producer, _reply_consumer) = unsafe { region.publisher_endpoints() }.into_parts();
    // SAFETY: this test attaches each channel role exactly once.
    let (_reply_producer, mut consumer) = unsafe { region.subscriber_endpoints() }.into_parts();

    producer.send(IvcMessageKind::Request, 1, b"one").unwrap();
    producer.send(IvcMessageKind::Request, 2, b"two").unwrap();

    let mut payload = [0; IVC_SLOT_PAYLOAD_SIZE];
    let first = consumer.try_recv(&mut payload).unwrap().unwrap();
    assert_eq!(first.kind(), IvcMessageKind::Request);
    assert_eq!(first.sequence(), 1);
    assert_eq!(&payload[..first.len()], b"one");

    let second = consumer.try_recv(&mut payload).unwrap().unwrap();
    assert_eq!(second.sequence(), 2);
    assert_eq!(&payload[..second.len()], b"two");
    assert_eq!(consumer.try_recv(&mut payload), Ok(None));
}

#[test]
fn ack_ring_is_independent_from_request_ring() {
    let mut region = new_region(PUBLISHER_VM_ID, CHANNEL_KEY);
    region.initialize();
    // SAFETY: this test attaches each channel role exactly once.
    let (mut request_producer, mut reply_consumer) =
        unsafe { region.publisher_endpoints() }.into_parts();
    // SAFETY: this test attaches each channel role exactly once.
    let (mut reply_producer, mut request_consumer) =
        unsafe { region.subscriber_endpoints() }.into_parts();

    request_producer
        .send(IvcMessageKind::Request, 9, b"request")
        .unwrap();
    reply_producer.send(IvcMessageKind::Ack, 9, b"ack").unwrap();

    let mut payload = [0; IVC_SLOT_PAYLOAD_SIZE];
    let ack = reply_consumer.try_recv(&mut payload).unwrap().unwrap();
    assert_eq!(ack.kind(), IvcMessageKind::Ack);
    assert_eq!(ack.sequence(), 9);
    assert_eq!(&payload[..ack.len()], b"ack");

    let request = request_consumer.try_recv(&mut payload).unwrap().unwrap();
    assert_eq!(request.kind(), IvcMessageKind::Request);
    assert_eq!(request.sequence(), 9);
}

#[test]
fn send_fails_when_ring_is_full() {
    let mut region = new_region(PUBLISHER_VM_ID, CHANNEL_KEY);
    region.initialize();
    // SAFETY: this test attaches the publisher role exactly once.
    let (mut producer, _consumer) = unsafe { region.publisher_endpoints() }.into_parts();

    for sequence in 0..IVC_RING_CAPACITY as u64 {
        producer
            .send(IvcMessageKind::Request, sequence, b"x")
            .unwrap();
    }

    assert_eq!(
        producer.send(IvcMessageKind::Request, IVC_RING_CAPACITY as u64, b"x"),
        Err(IvcRingError::Full)
    );
}

#[test]
fn endpoint_readiness_tracks_ring_occupancy_without_consuming() {
    let mut region = new_region(PUBLISHER_VM_ID, CHANNEL_KEY);
    region.initialize();
    // SAFETY: this test attaches each channel role exactly once.
    let (mut producer, _reply_consumer) = unsafe { region.publisher_endpoints() }.into_parts();
    // SAFETY: this test attaches each channel role exactly once.
    let (_reply_producer, mut consumer) = unsafe { region.subscriber_endpoints() }.into_parts();

    assert!(producer.can_send());
    assert!(!consumer.can_recv());

    for sequence in 0..IVC_RING_CAPACITY as u64 {
        producer
            .send(IvcMessageKind::Request, sequence, b"x")
            .unwrap();
    }
    assert!(!producer.can_send());
    assert!(consumer.can_recv());

    let mut payload = [0; IVC_SLOT_PAYLOAD_SIZE];
    let message = consumer.try_recv(&mut payload).unwrap().unwrap();
    assert_eq!(message.sequence(), 0);
    assert!(producer.can_send());
    assert!(consumer.can_recv());
}

#[test]
fn region_headers_survive_ring_wraparound() {
    let mut region = new_region(PUBLISHER_VM_ID, CHANNEL_KEY);
    region.initialize();
    // SAFETY: this test attaches each channel role exactly once.
    let (mut producer, _reply_consumer) = unsafe { region.publisher_endpoints() }.into_parts();
    // SAFETY: this test attaches each channel role exactly once.
    let (_reply_producer, mut consumer) = unsafe { region.subscriber_endpoints() }.into_parts();

    let mut payload = [0; IVC_SLOT_PAYLOAD_SIZE];
    for sequence in 0..(IVC_RING_CAPACITY * 4) as u64 {
        producer
            .send(IvcMessageKind::Request, sequence, b"x")
            .unwrap();
        let message = consumer.try_recv(&mut payload).unwrap().unwrap();
        assert_eq!(message.sequence(), sequence);
        assert!(region.channel_header_matches(PUBLISHER_VM_ID, CHANNEL_KEY));
        assert!(region.protocol_header_matches());
    }

    assert_eq!(consumer.try_recv(&mut payload), Ok(None));
}

#[test]
fn protocol_headers_survive_full_ring_cycles() {
    let mut region = new_region(PUBLISHER_VM_ID, CHANNEL_KEY);
    region.initialize();
    // SAFETY: this test attaches each channel role exactly once.
    let (mut producer, _reply_consumer) = unsafe { region.publisher_endpoints() }.into_parts();
    // SAFETY: this test attaches each channel role exactly once.
    let (_reply_producer, mut consumer) = unsafe { region.subscriber_endpoints() }.into_parts();

    let mut payload = [0; IVC_SLOT_PAYLOAD_SIZE];
    for cycle in 0..4 {
        for slot in 0..IVC_RING_CAPACITY {
            let sequence = (cycle * IVC_RING_CAPACITY + slot) as u64;
            producer
                .send(IvcMessageKind::Request, sequence, b"x")
                .unwrap();
        }
        assert_eq!(
            producer.send(IvcMessageKind::Request, u64::MAX, b"x"),
            Err(IvcRingError::Full)
        );

        for slot in 0..IVC_RING_CAPACITY {
            let sequence = (cycle * IVC_RING_CAPACITY + slot) as u64;
            let message = consumer.try_recv(&mut payload).unwrap().unwrap();
            assert_eq!(message.sequence(), sequence);
        }
        assert!(region.channel_header_matches(PUBLISHER_VM_ID, CHANNEL_KEY));
        assert!(region.protocol_header_matches());
    }
}

#[test]
fn bidirectional_endpoints_deliver_concurrent_streams() {
    use std::{boxed::Box, thread};

    const MESSAGES: u64 = 20_000;

    let region = Box::leak(Box::new(new_region(PUBLISHER_VM_ID, CHANNEL_KEY)));
    region.initialize();
    let region: &'static IvcRegion = region;
    // SAFETY: this test attaches each channel role exactly once.
    let (mut pub_producer, mut pub_consumer) = unsafe { region.publisher_endpoints() }.into_parts();
    // SAFETY: this test attaches each channel role exactly once.
    let (mut sub_producer, mut sub_consumer) =
        unsafe { region.subscriber_endpoints() }.into_parts();

    let publisher = thread::spawn(move || {
        let payload = [0xa5; IVC_SLOT_PAYLOAD_SIZE];
        let mut received = [0; IVC_SLOT_PAYLOAD_SIZE];
        for sequence in 0..MESSAGES {
            while pub_producer
                .send(IvcMessageKind::Request, sequence, &payload)
                .is_err()
            {
                thread::yield_now();
            }
            loop {
                if let Some(message) = pub_consumer.try_recv(&mut received).unwrap() {
                    assert_eq!(message.kind(), IvcMessageKind::Ack);
                    assert_eq!(message.sequence(), sequence);
                    break;
                }
                thread::yield_now();
            }
        }
    });

    let subscriber = thread::spawn(move || {
        let payload = [0x5a; IVC_SLOT_PAYLOAD_SIZE];
        let mut received = [0; IVC_SLOT_PAYLOAD_SIZE];
        for sequence in 0..MESSAGES {
            loop {
                if let Some(message) = sub_consumer.try_recv(&mut received).unwrap() {
                    assert_eq!(message.kind(), IvcMessageKind::Request);
                    assert_eq!(message.sequence(), sequence);
                    break;
                }
                thread::yield_now();
            }
            while sub_producer
                .send(IvcMessageKind::Ack, sequence, &payload)
                .is_err()
            {
                thread::yield_now();
            }
        }
    });

    publisher.join().expect("publisher thread panicked");
    subscriber.join().expect("subscriber thread panicked");
    assert!(region.channel_header_matches(PUBLISHER_VM_ID, CHANNEL_KEY));
    assert!(region.protocol_header_matches());
}

#[test]
fn send_rejects_payload_larger_than_one_slot() {
    let mut region = new_region(PUBLISHER_VM_ID, CHANNEL_KEY);
    region.initialize();
    // SAFETY: this test attaches each channel role exactly once.
    let (mut producer, _reply_consumer) = unsafe { region.publisher_endpoints() }.into_parts();
    // SAFETY: this test attaches each channel role exactly once.
    let (_reply_producer, mut consumer) = unsafe { region.subscriber_endpoints() }.into_parts();
    let payload = [0x5a; IVC_SLOT_PAYLOAD_SIZE + 1];

    assert_eq!(
        producer.send(IvcMessageKind::Request, 1, &payload),
        Err(IvcRingError::PayloadTooLarge {
            len: IVC_SLOT_PAYLOAD_SIZE + 1,
            capacity: IVC_SLOT_PAYLOAD_SIZE
        })
    );

    let mut received = [0; IVC_SLOT_PAYLOAD_SIZE];
    assert_eq!(consumer.try_recv(&mut received), Ok(None));
}

#[test]
fn recv_rejects_short_buffer_without_consuming_slot() {
    let mut region = new_region(PUBLISHER_VM_ID, CHANNEL_KEY);
    region.initialize();
    // SAFETY: this test attaches each channel role exactly once.
    let (mut producer, _reply_consumer) = unsafe { region.publisher_endpoints() }.into_parts();
    // SAFETY: this test attaches each channel role exactly once.
    let (_reply_producer, mut consumer) = unsafe { region.subscriber_endpoints() }.into_parts();

    producer
        .send(IvcMessageKind::Request, 7, b"payload")
        .unwrap();

    let mut short = [0u8; 3];
    assert_eq!(
        consumer.try_recv(&mut short),
        Err(IvcRingError::BufferTooSmall {
            required: 7,
            available: 3
        })
    );

    let mut full = [0u8; IVC_SLOT_PAYLOAD_SIZE];
    let message = consumer.try_recv(&mut full).unwrap().unwrap();
    assert_eq!(message.sequence(), 7);
    assert_eq!(message.len(), 7);
    assert_eq!(&full[..message.len()], b"payload");
    assert_eq!(consumer.try_recv(&mut full), Ok(None));
}

#[test]
fn protocol_region_fits_one_ivc_page() {
    assert!(core::mem::size_of::<IvcRegion>() <= CHANNEL_SIZE);
}

#[test]
fn peer_event_waiter_observes_recorded_irq_events_once() {
    let counter = AtomicU64::new(0);
    let waiter = IvcPeerEventWaiter::new(true, &counter);

    assert!(!waiter.observe_peer_event());
    record_peer_event(&counter);
    assert!(waiter.observe_peer_event());
    assert!(!waiter.observe_peer_event());
}

/// Regression test for the blocking review finding that the previous safe
/// `&self` API allowed two threads to produce on the same ring concurrently,
/// losing and tearing messages. With endpoint ownership, the only concurrent
/// pattern safe code can express is one producer plus one consumer; exercise
/// that pattern at volume and require every message to arrive exactly once,
/// in FIFO order, and intact.
#[test]
fn spsc_endpoints_deliver_all_messages_across_threads() {
    use std::{boxed::Box, thread, vec::Vec};

    const MESSAGES: u64 = 100_000;

    let region = Box::leak(Box::new(new_region(PUBLISHER_VM_ID, CHANNEL_KEY)));
    region.initialize();
    let region: &'static IvcRegion = region;
    // SAFETY: this test attaches each channel role exactly once.
    let (mut producer, _reply_consumer) = unsafe { region.publisher_endpoints() }.into_parts();
    // SAFETY: this test attaches each channel role exactly once.
    let (_reply_producer, mut consumer) = unsafe { region.subscriber_endpoints() }.into_parts();

    let producer_thread = thread::spawn(move || {
        let payload = [0x5a; IVC_SLOT_PAYLOAD_SIZE];
        for sequence in 0..MESSAGES {
            while producer
                .send(IvcMessageKind::Request, sequence, &payload)
                .is_err()
            {
                thread::yield_now();
            }
        }
    });

    let mut received: Vec<u64> = Vec::new();
    let mut payload = [0u8; IVC_SLOT_PAYLOAD_SIZE];
    loop {
        match consumer.try_recv(&mut payload) {
            Ok(Some(message)) => {
                assert!(payload.iter().all(|&byte| byte == 0x5a));
                received.push(message.sequence());
            }
            Ok(None) if producer_thread.is_finished() => break,
            Ok(None) => thread::yield_now(),
            Err(err) => panic!("recv failed: {err:?}"),
        }
    }
    producer_thread.join().expect("producer thread panicked");

    assert_eq!(received.len(), MESSAGES as usize, "messages were lost");
    for (expected, sequence) in received.iter().enumerate() {
        assert_eq!(
            *sequence, expected as u64,
            "messages must arrive in FIFO order"
        );
    }
}

fn new_region(publisher_id: usize, key: usize) -> IvcRegion {
    new_region_for_test(publisher_id, key)
}
