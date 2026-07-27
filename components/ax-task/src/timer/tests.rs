use alloc::boxed::Box;
use core::pin::Pin;

use super::*;

#[test]
fn expires_in_deadline_order_without_exceeding_the_batch() {
    let first = timer(1);
    let second = timer(2);
    let third = timer(3);
    let mut timers = TaskDeadlineQueue::new(4);
    unsafe {
        timers.arm(second.as_ref(), 20).unwrap();
        timers.arm(first.as_ref(), 10).unwrap();
        timers.arm(third.as_ref(), 30).unwrap();
    }
    let mut expired = [ExpiredTaskDeadline::EMPTY; 3];

    let result = timers.expire(TaskDeadlineExpireRequest::new(30, 2, 5), &mut expired);

    assert_eq!(result.processed(), 2);
    assert_eq!(result.expired(), 2);
    assert!(result.pending());
    assert_eq!(result.next_deadline_ns(), Some(35));
    assert_eq!(expired[0].thread(), Some(thread(1)));
    assert_eq!(expired[1].thread(), Some(thread(2)));
}

#[test]
fn reports_capacity_without_growing_the_heap() {
    let first = timer(1);
    let second = timer(2);
    let mut timers = TaskDeadlineQueue::new(1);
    unsafe { timers.arm(first.as_ref(), 10).unwrap() };

    assert_eq!(
        unsafe { timers.arm(second.as_ref(), 20) },
        Err(TaskDeadlineError::Capacity)
    );
    assert_eq!(timers.capacity(), 1);
}

#[test]
fn rearm_replaces_the_existing_entry_without_consuming_capacity() {
    let node = timer(7);
    let mut timers = TaskDeadlineQueue::new(1);
    let first = unsafe { timers.arm(node.as_ref(), 10).unwrap() };

    let second = unsafe { timers.arm(node.as_ref(), 20).unwrap() };

    assert_ne!(first, second);
    assert_eq!(timers.len(), 1);

    let mut expired = [ExpiredTaskDeadline::EMPTY; 1];
    let before_deadline = timers.expire(TaskDeadlineExpireRequest::new(10, 1, 1), &mut expired);
    assert_eq!(before_deadline.processed(), 0);
    assert_eq!(before_deadline.expired(), 0);
    assert!(!before_deadline.pending());
    assert_eq!(before_deadline.next_deadline_ns(), Some(20));

    let at_deadline = timers.expire(TaskDeadlineExpireRequest::new(20, 1, 1), &mut expired);
    assert_eq!(at_deadline.processed(), 1);
    assert_eq!(at_deadline.expired(), 1);
    assert_eq!(expired[0].token(), second);
}

#[test]
fn cancellation_removes_entry_and_reclaims_capacity_immediately() {
    let first = timer(11);
    let second = timer(22);
    let mut timers = TaskDeadlineQueue::new(1);
    let token = unsafe { timers.arm(first.as_ref(), 10).unwrap() };

    assert!(timers.cancel(first.as_ref(), token));
    assert!(timers.is_empty());
    assert!(unsafe { timers.arm(second.as_ref(), 20) }.is_ok());
}

#[test]
fn cancellation_rejects_the_non_arm_token() {
    let node = timer(23);
    let mut timers = TaskDeadlineQueue::new(1);

    assert!(
        !timers.cancel(node.as_ref(), TaskDeadlineToken::NONE),
        "the NONE sentinel must not identify an active arm operation"
    );
    assert!(timers.is_empty());

    let live = unsafe { timers.arm(node.as_ref(), 10).unwrap() };
    assert!(timers.cancel(node.as_ref(), live));
    assert!(!timers.cancel(node.as_ref(), TaskDeadlineToken::NONE));
}

#[test]
fn stale_generation_cancel_cannot_remove_the_rearmed_entry() {
    let node = timer(33);
    let mut timers = TaskDeadlineQueue::new(1);
    let stale = unsafe { timers.arm(node.as_ref(), 10).unwrap() };
    let live = unsafe { timers.arm(node.as_ref(), 20).unwrap() };

    assert!(!timers.cancel(node.as_ref(), stale));
    assert_eq!(timers.len(), 1);
    assert!(timers.cancel(node.as_ref(), live));
    assert!(timers.is_empty());
}

fn timer(slot: u32) -> Pin<Box<TaskDeadlineNode>> {
    Box::pin(TaskDeadlineNode::for_thread(thread(slot)))
}

const fn thread(slot: u32) -> crate::ThreadId {
    crate::ThreadId::from_parts(slot, 1)
}
