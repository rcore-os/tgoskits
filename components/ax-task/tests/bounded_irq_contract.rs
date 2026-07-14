use core::{
    pin::Pin,
    sync::atomic::{AtomicUsize, Ordering},
};

use ax_task::{
    CpuId, IrqNotifyResult, IrqRegisterResult, IrqWaitCell, IrqWaitRegistration, IrqWakeHandle,
    ThreadId,
    inbox::{InboxKind, InboxMessage, InboxNode, PublishResult, SchedulerInbox},
    timer::{ExpireRequest, ExpiredTimer, TimerNode, TimerQueue},
};

#[test]
fn timer_irq_work_is_bounded() {
    let mut queue = TimerQueue::new(3);
    for owner in 0..3 {
        unsafe { queue.arm(timer(owner), 10).unwrap() };
    }
    let mut output = [ExpiredTimer::EMPTY; 3];

    let batch = queue.expire(ExpireRequest::new(10, 2, 1), &mut output);

    assert_eq!(batch.processed(), 2);
    assert_eq!(batch.expired(), 2);
    assert!(batch.pending());
}

#[test]
fn remote_inbox_publication_coalesces_and_drain_is_bounded() {
    let inbox = SchedulerInbox::new(InboxKind::RemoteWake);
    let first = inbox_node(InboxKind::RemoteWake);
    let second = inbox_node(InboxKind::RemoteWake);
    let first_message = InboxMessage::remote_wake(thread(1), CpuId::new(1));
    let second_message = InboxMessage::remote_wake(thread(2), CpuId::new(1));
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

    let batch = inbox.drain(1, &mut output);

    assert_eq!(batch.drained(), 1);
    assert!(batch.pending());
    assert_eq!(output[0].thread_id(), thread(1));
}

#[test]
fn irq_before_register_is_delivered_to_the_single_waiter() {
    let cell = IrqWaitCell::new();
    let wakes = Box::leak(Box::new(AtomicUsize::new(0)));
    let wake = unsafe {
        // The counter and callback remain valid for the leaked registration.
        IrqWakeHandle::from_raw(wakes as *const AtomicUsize as usize, count_wake)
    };
    let registration: &'static IrqWaitRegistration =
        Box::leak(Box::new(IrqWaitRegistration::new(wake)));
    let registration = unsafe {
        // The leaked registration remains pinned for every wait-cell operation.
        Pin::new_unchecked(registration)
    };

    assert_eq!(cell.notify(), IrqNotifyResult::Pending);
    assert_eq!(
        cell.register(registration),
        IrqRegisterResult::ConsumedPending
    );
    assert_eq!(wakes.load(Ordering::Relaxed), 1);
}

fn timer(owner: usize) -> Pin<&'static TimerNode> {
    let node = Box::leak(Box::new(TimerNode::new(owner)));
    unsafe {
        // The leaked node outlives all queue entries.
        Pin::new_unchecked(node)
    }
}

fn inbox_node(kind: InboxKind) -> Pin<&'static InboxNode> {
    let node = Box::leak(Box::new(InboxNode::new(kind)));
    unsafe {
        // The leaked node outlives every inbox publication.
        Pin::new_unchecked(node)
    }
}

fn thread(slot: u32) -> ThreadId {
    ThreadId::from_parts(slot, 1)
}

/// Counts one direct IRQ wake.
///
/// # Safety
///
/// `data` must point to the leaked atomic installed by the test.
unsafe fn count_wake(data: usize) {
    let wakes = unsafe {
        // The test passes the leaked atomic address unchanged.
        &*(data as *const AtomicUsize)
    };
    wakes.fetch_add(1, Ordering::Relaxed);
}
