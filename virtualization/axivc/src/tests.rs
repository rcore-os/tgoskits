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
    let mut region = new_region();

    region.initialize(PUBLISHER_VM_ID, CHANNEL_KEY);

    assert!(region.channel_header_matches(PUBLISHER_VM_ID, CHANNEL_KEY));
    assert!(region.protocol_header_matches());
}

#[test]
fn request_ring_delivers_messages_in_fifo_order() {
    let mut region = new_region();
    region.initialize(PUBLISHER_VM_ID, CHANNEL_KEY);

    region.send_request(1, b"one").unwrap();
    region.send_request(2, b"two").unwrap();

    let mut payload = [0; IVC_SLOT_PAYLOAD_SIZE];
    let first = region.try_recv_request(&mut payload).unwrap().unwrap();
    assert_eq!(first.kind(), IvcMessageKind::Request);
    assert_eq!(first.sequence(), 1);
    assert_eq!(&payload[..first.len()], b"one");

    let second = region.try_recv_request(&mut payload).unwrap().unwrap();
    assert_eq!(second.sequence(), 2);
    assert_eq!(&payload[..second.len()], b"two");
    assert_eq!(region.try_recv_request(&mut payload), Ok(None));
}

#[test]
fn ack_ring_is_independent_from_request_ring() {
    let mut region = new_region();
    region.initialize(PUBLISHER_VM_ID, CHANNEL_KEY);

    region.send_request(9, b"request").unwrap();
    region.send_ack(9, b"ack").unwrap();

    let mut payload = [0; IVC_SLOT_PAYLOAD_SIZE];
    let ack = region.try_recv_ack(&mut payload).unwrap().unwrap();
    assert_eq!(ack.kind(), IvcMessageKind::Ack);
    assert_eq!(ack.sequence(), 9);
    assert_eq!(&payload[..ack.len()], b"ack");

    let request = region.try_recv_request(&mut payload).unwrap().unwrap();
    assert_eq!(request.kind(), IvcMessageKind::Request);
    assert_eq!(request.sequence(), 9);
}

#[test]
fn send_fails_when_ring_is_full() {
    let mut region = new_region();
    region.initialize(PUBLISHER_VM_ID, CHANNEL_KEY);

    for sequence in 0..IVC_RING_CAPACITY as u64 {
        region.send_request(sequence, b"x").unwrap();
    }

    assert_eq!(
        region.send_request(IVC_RING_CAPACITY as u64, b"x"),
        Err(IvcRingError::Full)
    );
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

fn new_region() -> IvcRegion {
    new_region_for_test()
}
