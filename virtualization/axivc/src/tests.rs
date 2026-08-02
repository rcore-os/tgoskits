use core::sync::atomic::AtomicU64;

use crate::{
    IVC_CELL_FRAGMENT_CAPACITY, IVC_CELL_SIZE, IVC_REGION_VERSION, IvcMessageError,
    IvcMessageReceiver, IvcPeerEventWaiter, IvcRegion, record_peer_event,
    region::new_region_for_test,
};

const CHANNEL_KEY: usize = 0x4956_4301;
const PUBLISHER_VM_ID: usize = 1;
const CHANNEL_SIZE: usize = 4096;

#[test]
fn region_header_and_channel_header_match_after_initialize() {
    let mut region = new_region(PUBLISHER_VM_ID, CHANNEL_KEY);

    region.initialize();

    assert_eq!(IVC_REGION_VERSION, 3);
    assert!(region.channel_header_matches(PUBLISHER_VM_ID, CHANNEL_KEY));
    assert!(region.protocol_header_matches());
}

#[test]
fn message_boundaries_cover_empty_single_and_multi_cell_payloads() {
    for length in [0, 1, 39, 40, 41, 640, 641] {
        let payload: std::vec::Vec<u8> = (0..length).map(|index| index as u8).collect();
        let received = transfer_one_message(&payload);
        assert_eq!(received, payload, "length {length}");
    }
}

#[test]
fn a_message_larger_than_the_ring_streams_across_many_wraps() {
    let payload: std::vec::Vec<u8> = (0..1024 * 1024)
        .map(|index| (index as u8).wrapping_mul(31))
        .collect();

    let received = transfer_one_message(&payload);

    assert_eq!(received, payload);
}

#[test]
fn exact_fragment_multiples_publish_last_without_an_extra_cell() {
    let region = initialized_region();
    let (mut sender, _publisher_receiver) = unsafe { region.publisher_endpoints() }.into_parts();
    let (_subscriber_sender, mut receiver) = unsafe { region.subscriber_endpoints() }.into_parts();
    let payload = [0x5a; IVC_CELL_FRAGMENT_CAPACITY * 2];

    sender.start_message(payload.len() as u64).unwrap();
    let sent = sender.try_write(&payload).unwrap();
    assert_eq!(sent.published_cells(), 2);
    assert!(sent.is_complete());

    let mut output = [0u8; IVC_CELL_FRAGMENT_CAPACITY * 2];
    let received = receiver.try_read(&mut output).unwrap();
    assert_eq!(received.consumed_cells(), 2);
    assert!(received.is_complete());
    assert_eq!(output, payload);
}

#[test]
fn receive_does_not_consume_fragment_when_output_is_too_small() {
    let region = initialized_region();
    let (mut sender, _publisher_receiver) = unsafe { region.publisher_endpoints() }.into_parts();
    let (_subscriber_sender, mut receiver) = unsafe { region.subscriber_endpoints() }.into_parts();
    let expected = [0x5a; IVC_CELL_FRAGMENT_CAPACITY + 8];

    sender.start_message(expected.len() as u64).unwrap();
    assert!(sender.try_write(&expected).unwrap().is_complete());

    let mut undersized = [0u8; 8];
    assert_eq!(
        receiver.try_read(&mut undersized),
        Err(IvcMessageError::BufferTooSmall {
            required: IVC_CELL_FRAGMENT_CAPACITY,
            provided: undersized.len(),
        })
    );

    let meta = receiver.peek_message_meta().unwrap().unwrap();
    assert_eq!(meta.len(), expected.len() as u64);
    let mut output = [0u8; IVC_CELL_FRAGMENT_CAPACITY];
    let first = receiver.try_read(&mut output).unwrap();
    assert_eq!(first.written(), IVC_CELL_FRAGMENT_CAPACITY);
    assert!(!first.is_complete());
    assert_eq!(output, expected[..IVC_CELL_FRAGMENT_CAPACITY]);

    let last = receiver.try_read(&mut output).unwrap();
    assert_eq!(last.written(), 8);
    assert!(last.is_complete());
    assert_eq!(&output[..8], &expected[IVC_CELL_FRAGMENT_CAPACITY..]);
}

#[test]
fn sender_accepts_input_in_multiple_calls_and_rejects_excess_input() {
    let region = initialized_region();
    let (mut sender, _publisher_receiver) = unsafe { region.publisher_endpoints() }.into_parts();
    let (_subscriber_sender, mut receiver) = unsafe { region.subscriber_endpoints() }.into_parts();

    sender.start_message(5).unwrap();
    assert_eq!(
        sender.try_write(b"abcdef"),
        Err(IvcMessageError::InputExceedsRemaining {
            remaining: 5,
            provided: 6,
        })
    );
    assert_eq!(sender.try_write(b"ab").unwrap().consumed(), 2);
    assert!(sender.try_write(b"cde").unwrap().is_complete());

    let mut output = [0u8; IVC_CELL_FRAGMENT_CAPACITY];
    let received = receiver.try_read(&mut output).unwrap();
    assert!(received.is_complete());
    assert_eq!(&output[..received.written()], b"abcde");
}

#[test]
fn abort_terminates_a_partial_message_and_allows_the_next_message() {
    let region = initialized_region();
    let (mut sender, _publisher_receiver) = unsafe { region.publisher_endpoints() }.into_parts();
    let (_subscriber_sender, mut receiver) = unsafe { region.subscriber_endpoints() }.into_parts();

    sender.start_message(80).unwrap();
    sender.try_write(&[0x11; 40]).unwrap();
    sender.try_abort().unwrap();

    let mut output = [0u8; 80];
    let partial = receiver.try_read(&mut output).unwrap();
    assert_eq!(partial.written(), 40);
    assert!(!partial.is_complete());
    assert_eq!(
        receiver.try_read(&mut output),
        Err(IvcMessageError::TransferAborted)
    );

    sender.start_message(2).unwrap();
    assert!(sender.try_write(b"ok").unwrap().is_complete());
    let received = receiver.try_read(&mut output).unwrap();
    assert!(received.is_complete());
    assert_eq!(&output[..received.written()], b"ok");
}

#[test]
fn malformed_frame_is_reported_and_not_silently_consumed() {
    use crate::{
        endpoint::{IvcCellConsumer, IvcCellProducer},
        ring::{IvcRingDirection, new_ring_for_test},
    };

    let ring = new_ring_for_test();
    ring.initialize(IvcRingDirection::PublisherToSubscriber);
    let mut raw_producer = IvcCellProducer::new(&ring);
    let mut receiver = IvcMessageReceiver::new(IvcCellConsumer::new(&ring));
    let cell = [0u8; IVC_CELL_SIZE];
    raw_producer.try_push_cell(&cell).unwrap();

    let expected = IvcMessageError::UnsupportedVersion { version: 0 };
    assert_eq!(receiver.peek_message_meta(), Err(expected));
    assert_eq!(receiver.try_discard(), Err(expected));
}

#[test]
fn opposite_ring_directions_deliver_independent_messages() {
    let region = initialized_region();
    let (mut publisher_sender, mut publisher_receiver) =
        unsafe { region.publisher_endpoints() }.into_parts();
    let (mut subscriber_sender, mut subscriber_receiver) =
        unsafe { region.subscriber_endpoints() }.into_parts();

    publisher_sender.start_message(7).unwrap();
    subscriber_sender.start_message(10).unwrap();
    assert!(
        publisher_sender
            .try_write(b"request")
            .unwrap()
            .is_complete()
    );
    assert!(
        subscriber_sender
            .try_write(b"reply-data")
            .unwrap()
            .is_complete()
    );

    let mut output = [0u8; IVC_CELL_FRAGMENT_CAPACITY];
    let reply = publisher_receiver.try_read(&mut output).unwrap();
    assert_eq!(&output[..reply.written()], b"reply-data");
    let request = subscriber_receiver.try_read(&mut output).unwrap();
    assert_eq!(&output[..request.written()], b"request");
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

#[test]
fn spsc_message_endpoints_deliver_all_messages_across_threads() {
    use std::{boxed::Box, thread};

    const MESSAGES: u64 = 100_000;

    let region = Box::leak(Box::new(new_region(PUBLISHER_VM_ID, CHANNEL_KEY)));
    region.initialize();
    let region: &'static IvcRegion = region;
    let (mut sender, _publisher_receiver) = unsafe { region.publisher_endpoints() }.into_parts();
    let (_subscriber_sender, mut receiver) = unsafe { region.subscriber_endpoints() }.into_parts();

    let sender_thread = thread::spawn(move || {
        for value in 0..MESSAGES {
            sender.start_message(8).unwrap();
            let payload = value.to_le_bytes();
            while !sender.try_write(&payload).unwrap().is_complete() {
                thread::yield_now();
            }
        }
    });

    let mut expected = 0u64;
    let mut output = [0u8; IVC_CELL_FRAGMENT_CAPACITY];
    while expected < MESSAGES {
        match receiver.try_read(&mut output) {
            Ok(progress) if progress.is_complete() => {
                let value = u64::from_le_bytes(output[..8].try_into().unwrap());
                assert_eq!(value, expected);
                expected += 1;
            }
            Ok(_) => thread::yield_now(),
            Err(error) => panic!("receive failed: {error:?}"),
        }
    }
    sender_thread.join().expect("sender thread panicked");
}

fn transfer_one_message(payload: &[u8]) -> std::vec::Vec<u8> {
    let region = initialized_region();
    let (mut sender, _publisher_receiver) = unsafe { region.publisher_endpoints() }.into_parts();
    let (_subscriber_sender, mut receiver) = unsafe { region.subscriber_endpoints() }.into_parts();
    sender.start_message(payload.len() as u64).unwrap();

    let mut sent = 0;
    let mut send_complete = false;
    let mut receive_complete = false;
    let mut received = std::vec::Vec::with_capacity(payload.len());
    let mut output = [0u8; IVC_CELL_FRAGMENT_CAPACITY * 4];
    while !send_complete || !receive_complete {
        if !send_complete {
            let progress = sender.try_write(&payload[sent..]).unwrap();
            sent += progress.consumed();
            send_complete = progress.is_complete();
        }
        if !receive_complete {
            let progress = receiver.try_read(&mut output).unwrap();
            received.extend_from_slice(&output[..progress.written()]);
            receive_complete = progress.is_complete();
        }
    }
    assert_eq!(sent, payload.len());
    received
}

fn initialized_region() -> IvcRegion {
    let mut region = new_region(PUBLISHER_VM_ID, CHANNEL_KEY);
    region.initialize();
    region
}

fn new_region(publisher_id: usize, key: usize) -> IvcRegion {
    new_region_for_test(publisher_id, key)
}
