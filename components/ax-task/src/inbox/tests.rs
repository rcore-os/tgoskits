use alloc::{boxed::Box, sync::Arc};
use core::pin::Pin;

use super::*;
use crate::{CpuId, ThreadId};

#[test]
fn coalesces_duplicate_publication_and_preserves_fifo_order() {
    let inbox = SchedulerInbox::new(InboxKind::RemoteWake);
    let first = node(InboxKind::RemoteWake);
    let second = node(InboxKind::RemoteWake);
    let first_message = InboxMessage::remote_wake(thread(1), CpuId::new(2));
    let second_message = InboxMessage::remote_wake(thread(2), CpuId::new(2));

    assert_eq!(
        inbox.publish(first.pin(), first_message),
        PublishResult::Published
    );
    assert_eq!(
        inbox.publish(first.pin(), first_message),
        PublishResult::AlreadyPending
    );
    assert_eq!(
        inbox.publish(second.pin(), second_message),
        PublishResult::Published
    );
    let mut output = [InboxMessage::EMPTY; 2];

    let drained = inbox.drain(2, &mut output);

    assert_eq!(drained.drained(), 2);
    assert!(!drained.pending());
    assert_eq!(output[0].thread_id(), thread(1));
    assert_eq!(output[1].thread_id(), thread(2));
}

#[test]
fn bounds_each_drain_and_reports_remaining_work() {
    let inbox = SchedulerInbox::new(InboxKind::Migration);
    let nodes = [
        node(InboxKind::Migration),
        node(InboxKind::Migration),
        node(InboxKind::Migration),
    ];
    for (index, node) in nodes.iter().enumerate() {
        let message =
            InboxMessage::migration(thread(index as u32), CpuId::new(0), CpuId::new(1), 9);
        assert_eq!(inbox.publish(node.pin(), message), PublishResult::Published);
    }
    let mut output = [InboxMessage::EMPTY; 3];

    let first = inbox.drain(2, &mut output);
    assert_eq!(first.drained(), 2);
    assert!(first.pending());
    assert_eq!(first.remainder(), DrainRemainder::MoreReady);
    let second = inbox.drain(2, &mut output);
    assert_eq!(second.drained(), 1);
    assert!(!second.pending());
}

#[test]
fn rejects_a_node_from_a_different_inbox_class() {
    let inbox = SchedulerInbox::new(InboxKind::Reclaim);
    let wake_node = node(InboxKind::RemoteWake);
    let message = InboxMessage::reclaim(thread(1), 4, 0x1234);

    assert_eq!(
        inbox.publish(wake_node.pin(), message),
        PublishResult::WrongKind
    );
}

#[test]
fn defers_detach_while_a_publisher_retains_the_observed_head() {
    let inbox = Arc::new(SchedulerInbox::new(InboxKind::RemoteWake));
    let first = node(InboxKind::RemoteWake);
    let second = node(InboxKind::RemoteWake);
    assert_eq!(
        inbox.publish(
            first.pin(),
            InboxMessage::remote_wake(thread(1), CpuId::new(0)),
        ),
        PublishResult::Published
    );

    inbox.arm_test_publisher_pause();
    let publisher_inbox = Arc::clone(&inbox);
    let second_pin = second.pin();
    let publisher = std::thread::spawn(move || {
        publisher_inbox.publish(
            second_pin,
            InboxMessage::remote_wake(thread(2), CpuId::new(0)),
        )
    });
    inbox.wait_for_test_publisher_pause();

    let mut output = [InboxMessage::EMPTY; 2];
    let while_observed = inbox.drain(2, &mut output);
    inbox.resume_test_publisher();
    assert_eq!(publisher.join().unwrap(), PublishResult::Published);

    let after_publish = inbox.drain(2, &mut output);
    assert_eq!(
        while_observed.drained() + after_publish.drained(),
        2,
        "the fixture must release both intrusive memberships"
    );
    assert_eq!(
        while_observed.drained(),
        0,
        "the consumer must not detach a head whose address and provenance are still retained by a \
         publisher"
    );
    assert!(
        while_observed.pending(),
        "deferred grace must remain visible to the next bounded drain"
    );
    assert_eq!(
        while_observed.remainder(),
        DrainRemainder::PublisherInFlight
    );
}

#[test]
fn new_generation_entrant_does_not_delay_retired_head_grace() {
    let inbox = Arc::new(SchedulerInbox::new(InboxKind::RemoteWake));
    let first = node(InboxKind::RemoteWake);
    let retiring_tail = node(InboxKind::RemoteWake);
    let next_generation = node(InboxKind::RemoteWake);
    assert_eq!(
        inbox.publish(
            first.pin(),
            InboxMessage::remote_wake(thread(1), CpuId::new(0)),
        ),
        PublishResult::Published
    );

    inbox.arm_test_publisher_pause();
    let retiring_inbox = Arc::clone(&inbox);
    let retiring_pin = retiring_tail.pin();
    let retiring_publisher = std::thread::spawn(move || {
        retiring_inbox.publish(
            retiring_pin,
            InboxMessage::remote_wake(thread(2), CpuId::new(0)),
        )
    });
    inbox.wait_for_test_publisher_pause();
    let mut output = [InboxMessage::EMPTY; 2];
    let grace_started = inbox.drain(2, &mut output);
    inbox.resume_test_publisher();
    assert_eq!(retiring_publisher.join().unwrap(), PublishResult::Published);

    inbox.arm_test_generation_pause();
    let next_inbox = Arc::clone(&inbox);
    let next_pin = next_generation.pin();
    let next_publisher = std::thread::spawn(move || {
        next_inbox.publish(
            next_pin,
            InboxMessage::remote_wake(thread(3), CpuId::new(0)),
        )
    });
    inbox.wait_for_test_generation_pause();
    let retired = inbox.drain(2, &mut output);
    inbox.resume_test_generation_publisher();
    assert_eq!(next_publisher.join().unwrap(), PublishResult::Published);

    let next = inbox.drain(2, &mut output);
    assert_eq!(grace_started.drained(), 0);
    assert!(grace_started.pending());
    assert_eq!(grace_started.remainder(), DrainRemainder::PublisherInFlight);
    assert_eq!(
        retired.drained(),
        2,
        "an entrant that has sampled only the new generation must not pin the retired head"
    );
    assert_eq!(next.drained(), 1);
    assert!(!next.pending());
}

struct TestInboxNode(Pin<Box<InboxNode>>);

impl TestInboxNode {
    fn pin(&self) -> Pin<&'static InboxNode> {
        let node = self.0.as_ref().get_ref() as *const InboxNode;
        unsafe {
            // The fixture owns a pinned allocation. Every test drains or rejects
            // its publication before this fixture is dropped.
            Pin::new_unchecked(&*node)
        }
    }
}

fn node(kind: InboxKind) -> TestInboxNode {
    TestInboxNode(Box::pin(InboxNode::new(kind)))
}

fn thread(slot: u32) -> ThreadId {
    ThreadId::from_parts(slot, 1)
}
