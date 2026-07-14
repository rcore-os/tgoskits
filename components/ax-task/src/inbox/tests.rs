use alloc::boxed::Box;
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
        inbox.publish(first, first_message),
        PublishResult::Published
    );
    assert_eq!(
        inbox.publish(first, first_message),
        PublishResult::AlreadyPending
    );
    assert_eq!(
        inbox.publish(second, second_message),
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
    for (index, node) in nodes.into_iter().enumerate() {
        let message =
            InboxMessage::migration(thread(index as u32), CpuId::new(0), CpuId::new(1), 9);
        assert_eq!(inbox.publish(node, message), PublishResult::Published);
    }
    let mut output = [InboxMessage::EMPTY; 3];

    let first = inbox.drain(2, &mut output);
    assert_eq!(first.drained(), 2);
    assert!(first.pending());
    let second = inbox.drain(2, &mut output);
    assert_eq!(second.drained(), 1);
    assert!(!second.pending());
}

#[test]
fn rejects_a_node_from_a_different_inbox_class() {
    let inbox = SchedulerInbox::new(InboxKind::Reclaim);
    let wake_node = node(InboxKind::RemoteWake);
    let message = InboxMessage::reclaim(thread(1), 4, 0x1234);

    assert_eq!(inbox.publish(wake_node, message), PublishResult::WrongKind);
}

fn node(kind: InboxKind) -> Pin<&'static InboxNode> {
    let node = Box::leak(Box::new(InboxNode::new(kind)));
    unsafe {
        // The leaked intrusive node remains pinned for the inbox lifetime.
        Pin::new_unchecked(node)
    }
}

fn thread(slot: u32) -> ThreadId {
    ThreadId::from_parts(slot, 1)
}
