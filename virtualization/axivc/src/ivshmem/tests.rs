use std::{
    boxed::Box,
    mem::{offset_of, size_of},
    vec::Vec,
};

use super::{
    IVSHMEM_MESSAGE_CHANNEL_SIZE, IvshmemMessageError, IvshmemMessageHeaderLayout,
    IvshmemMessagePort, IvshmemSessionState,
};
use crate::{IVC_RING_CAPACITY, IVC_SLOT_FRAGMENT_CAPACITY, IvcEndpoints, IvcMessageError};

#[repr(C, align(256))]
struct TestOutput([u8; IVSHMEM_MESSAGE_CHANNEL_SIZE]);

impl TestOutput {
    fn new() -> Box<Self> {
        Box::new(Self([0; IVSHMEM_MESSAGE_CHANNEL_SIZE]))
    }

    fn address(&mut self) -> *mut u8 {
        self.0.as_mut_ptr()
    }
}

#[test]
fn message_v1_flows_full_duplex_with_backpressure_and_wrap() {
    assert_eq!(IVSHMEM_MESSAGE_CHANNEL_SIZE, 8448);

    let mut left_output = TestOutput::new();
    let mut right_output = TestOutput::new();
    let left_address = left_output.address();
    let right_address = right_output.address();
    // SAFETY: both test outputs live through every derived port and endpoint,
    // and each output has one writer.
    let (mut left_port, mut right_port) = unsafe { attach_pair(left_address, right_address) };
    let (left_endpoints, right_endpoints) = start_and_connect(&mut left_port, &mut right_port);
    let (mut left_sender, mut left_receiver) = left_endpoints.into_parts();
    let (mut right_sender, mut right_receiver) = right_endpoints.into_parts();

    right_sender.start_message(7).unwrap();
    assert!(right_sender.try_write(b"reply!!").unwrap().is_complete());
    let mut reply = [0u8; IVC_SLOT_FRAGMENT_CAPACITY];
    let reply_progress = left_receiver.try_read(&mut reply).unwrap();
    assert!(reply_progress.is_complete());
    assert_eq!(&reply[..reply_progress.written()], b"reply!!");

    let ring_payload = IVC_RING_CAPACITY * IVC_SLOT_FRAGMENT_CAPACITY;
    let payload: Vec<u8> = (0..ring_payload * 2 + 1)
        .map(|index| (index as u8).wrapping_mul(17))
        .collect();
    left_sender.start_message(payload.len() as u64).unwrap();

    let first = left_sender.try_write(&payload).unwrap();
    assert_eq!(first.published_slots(), IVC_RING_CAPACITY);
    assert_eq!(first.consumed(), ring_payload);
    assert!(!first.is_complete());

    let blocked = left_sender.try_write(&payload[first.consumed()..]).unwrap();
    assert_eq!(blocked.consumed(), 0);
    assert_eq!(blocked.published_slots(), 0);
    assert!(!blocked.is_complete());

    let mut output = [0u8; IVC_RING_CAPACITY * IVC_SLOT_FRAGMENT_CAPACITY];
    let drained = right_receiver.try_read(&mut output).unwrap();
    assert_eq!(drained.consumed_slots(), IVC_RING_CAPACITY);
    assert_eq!(drained.written(), ring_payload);
    assert!(!drained.is_complete());

    let resumed = left_sender.try_write(&payload[first.consumed()..]).unwrap();
    assert!(resumed.published_slots() > 0);
    assert!(!resumed.is_complete());

    let mut sent = first.consumed() + resumed.consumed();
    let mut received = payload[..drained.written()].to_vec();
    let mut complete = false;
    for _ in 0..128 {
        let receive = right_receiver.try_read(&mut output).unwrap();
        received.extend_from_slice(&output[..receive.written()]);
        complete = receive.is_complete();
        if complete {
            break;
        }
        if sent < payload.len() {
            let send = left_sender.try_write(&payload[sent..]).unwrap();
            sent += send.consumed();
        }
    }
    assert!(complete);
    assert_eq!(sent, payload.len());
    assert_eq!(received, payload);
    assert_eq!(left_sender.available_slots(), IVC_RING_CAPACITY);
    assert!(!right_receiver.can_receive());
}

#[test]
fn abort_after_partial_publication_allows_a_fragmented_follow_up_record() {
    let mut left_output = TestOutput::new();
    let mut right_output = TestOutput::new();
    let left_address = left_output.address();
    let right_address = right_output.address();
    // SAFETY: both test outputs live through every derived port and endpoint,
    // and each output has one writer.
    let (mut left_port, mut right_port) = unsafe { attach_pair(left_address, right_address) };
    let (left_endpoints, right_endpoints) = start_and_connect(&mut left_port, &mut right_port);
    let (mut left_sender, _left_receiver) = left_endpoints.into_parts();
    let (_right_sender, mut right_receiver) = right_endpoints.into_parts();

    let cancelled: Vec<u8> = (0..IVC_SLOT_FRAGMENT_CAPACITY + 1)
        .map(|index| (index as u8).wrapping_mul(29))
        .collect();
    left_sender.start_message(cancelled.len() as u64).unwrap();
    let first = left_sender
        .try_write(&cancelled[..IVC_SLOT_FRAGMENT_CAPACITY])
        .unwrap();
    assert_eq!(first.published_slots(), 1);
    assert_eq!(first.consumed(), IVC_SLOT_FRAGMENT_CAPACITY);
    assert!(!first.is_complete());
    assert_eq!(left_sender.available_slots(), IVC_RING_CAPACITY - 1);

    left_sender.try_abort().unwrap();
    assert_eq!(left_sender.available_slots(), IVC_RING_CAPACITY - 2);

    let follow_up: Vec<u8> = (0..IVC_SLOT_FRAGMENT_CAPACITY + 1)
        .map(|index| (index as u8).wrapping_mul(53))
        .collect();
    left_sender.start_message(follow_up.len() as u64).unwrap();
    let published = left_sender.try_write(&follow_up).unwrap();
    assert_eq!(published.published_slots(), 2);
    assert_eq!(published.consumed(), follow_up.len());
    assert!(published.is_complete());

    let mut output = [0u8; IVC_SLOT_FRAGMENT_CAPACITY + 1];
    let partial = right_receiver.try_read(&mut output).unwrap();
    assert_eq!(partial.consumed_slots(), 1);
    assert_eq!(partial.written(), IVC_SLOT_FRAGMENT_CAPACITY);
    assert!(!partial.is_complete());
    assert_eq!(
        &output[..partial.written()],
        &cancelled[..IVC_SLOT_FRAGMENT_CAPACITY]
    );
    assert!(right_receiver.can_receive());

    assert_eq!(
        right_receiver.try_read(&mut output),
        Err(IvcMessageError::TransferAborted)
    );
    assert!(right_receiver.can_receive());

    let complete = right_receiver.try_read(&mut output).unwrap();
    assert_eq!(complete.consumed_slots(), 2);
    assert_eq!(complete.written(), follow_up.len());
    assert!(complete.is_complete());
    assert_eq!(&output[..complete.written()], follow_up);
    assert!(!right_receiver.can_receive());
    assert_eq!(left_sender.available_slots(), IVC_RING_CAPACITY);
}

#[test]
fn peer_restart_rejects_old_data_and_allows_a_new_session() {
    let mut left_output = TestOutput::new();
    let mut right_output = TestOutput::new();
    let left_address = left_output.address();
    let right_address = right_output.address();
    // SAFETY: both test outputs live through every derived port and endpoint,
    // and each output has one writer.
    let (mut left_port, mut right_port) = unsafe { attach_pair(left_address, right_address) };
    let (left_endpoints, right_endpoints) = start_and_connect(&mut left_port, &mut right_port);
    let (mut left_sender, left_receiver) = left_endpoints.into_parts();
    let (right_sender, mut right_receiver) = right_endpoints.into_parts();

    left_sender.start_message(3).unwrap();
    assert!(left_sender.try_write(b"old").unwrap().is_complete());

    drop((left_sender, left_receiver, left_port));

    // SAFETY: the old left endpoints are gone and this is the new sole writer
    // for the persisted left output backing.
    let mut new_left_port = unsafe {
        IvshmemMessagePort::attach(
            left_address,
            IVSHMEM_MESSAGE_CHANNEL_SIZE,
            right_address.cast_const(),
            IVSHMEM_MESSAGE_CHANNEL_SIZE,
        )
    }
    .unwrap();
    // SAFETY: this guest restarted the left lane after quiescing its endpoints.
    let new_left_incarnation = unsafe { new_left_port.begin_session() }.unwrap();
    assert_eq!(new_left_incarnation.get(), 2);
    assert!(matches!(
        right_port.session_state().unwrap(),
        IvshmemSessionState::PeerChanged {
            observed_peer: Some(observed),
            ..
        } if observed == new_left_incarnation
    ));

    let mut stale_output = [0u8; IVC_SLOT_FRAGMENT_CAPACITY];
    assert_eq!(
        right_receiver.try_read(&mut stale_output),
        Err(IvcMessageError::SessionLost)
    );

    drop((right_sender, right_receiver, right_port));
    // SAFETY: the old right endpoints are gone and this is the new sole writer
    // for the persisted right output backing.
    let mut new_right_port = unsafe {
        IvshmemMessagePort::attach(
            right_address,
            IVSHMEM_MESSAGE_CHANNEL_SIZE,
            left_address.cast_const(),
            IVSHMEM_MESSAGE_CHANNEL_SIZE,
        )
    }
    .unwrap();
    // SAFETY: this guest restarted the right lane after quiescing its endpoints.
    let new_right_incarnation = unsafe { new_right_port.begin_session() }.unwrap();
    assert_eq!(new_right_incarnation.get(), 2);

    let (new_left_endpoints, new_right_endpoints) =
        poll_and_connect(&mut new_left_port, &mut new_right_port);
    let (mut new_left_sender, _new_left_receiver) = new_left_endpoints.into_parts();
    let (_new_right_sender, mut new_right_receiver) = new_right_endpoints.into_parts();
    new_left_sender.start_message(3).unwrap();
    assert!(new_left_sender.try_write(b"new").unwrap().is_complete());
    let progress = new_right_receiver.try_read(&mut stale_output).unwrap();
    assert!(progress.is_complete());
    assert_eq!(&stale_output[..progress.written()], b"new");
}

#[test]
fn incarnation_never_wraps_to_zero() {
    let mut left_output = TestOutput::new();
    let mut right_output = TestOutput::new();
    let incarnation_offset = offset_of!(IvshmemMessageHeaderLayout, incarnation);
    left_output.0[incarnation_offset..incarnation_offset + size_of::<u32>()]
        .copy_from_slice(&u32::MAX.to_ne_bytes());
    let left_address = left_output.address();
    let right_address = right_output.address();
    // SAFETY: both test outputs are live and distinct.
    let (mut left_port, _right_port) = unsafe { attach_pair(left_address, right_address) };

    // SAFETY: this test owns the fresh left output and has no live endpoints.
    assert_eq!(
        unsafe { left_port.begin_session() },
        Err(IvshmemMessageError::IncarnationExhausted { current: u32::MAX })
    );
}

unsafe fn attach_pair<'a>(
    left_address: *mut u8,
    right_address: *mut u8,
) -> (IvshmemMessagePort<'a>, IvshmemMessagePort<'a>) {
    // SAFETY: both test outputs are distinct, 256-byte-aligned allocations that
    // outlive the returned ports. This helper's caller guarantees each output
    // has exactly one protocol writer.
    let left_port = unsafe {
        IvshmemMessagePort::attach(
            left_address,
            IVSHMEM_MESSAGE_CHANNEL_SIZE,
            right_address.cast_const(),
            IVSHMEM_MESSAGE_CHANNEL_SIZE,
        )
    }
    .unwrap();
    // SAFETY: symmetric view of the same two live test outputs.
    let right_port = unsafe {
        IvshmemMessagePort::attach(
            right_address,
            IVSHMEM_MESSAGE_CHANNEL_SIZE,
            left_address.cast_const(),
            IVSHMEM_MESSAGE_CHANNEL_SIZE,
        )
    }
    .unwrap();
    (left_port, right_port)
}

fn start_and_connect<'a>(
    left: &mut IvshmemMessagePort<'a>,
    right: &mut IvshmemMessagePort<'a>,
) -> (IvcEndpoints<'a>, IvcEndpoints<'a>) {
    // SAFETY: both ports are fresh and no endpoint from an earlier generation
    // is alive.
    let left_incarnation = unsafe { left.begin_session() }.unwrap();
    assert_eq!(left_incarnation.get(), 1);
    assert_eq!(
        left.poll_handshake().unwrap(),
        IvshmemSessionState::WaitingForPeer {
            local: left_incarnation,
        }
    );

    // SAFETY: this is the symmetric fresh peer output region.
    let right_incarnation = unsafe { right.begin_session() }.unwrap();
    assert_eq!(right_incarnation.get(), 1);
    poll_and_connect(left, right)
}

fn poll_and_connect<'a>(
    left: &mut IvshmemMessagePort<'a>,
    right: &mut IvshmemMessagePort<'a>,
) -> (IvcEndpoints<'a>, IvcEndpoints<'a>) {
    for _ in 0..4 {
        let left_state = left.poll_handshake().unwrap();
        let right_state = right.poll_handshake().unwrap();
        if matches!(left_state, IvshmemSessionState::Ready { .. })
            && matches!(right_state, IvshmemSessionState::Ready { .. })
        {
            return (left.try_connect().unwrap(), right.try_connect().unwrap());
        }
    }
    panic!("handshake did not converge");
}
