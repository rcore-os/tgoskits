use alloc::boxed::Box;
use core::pin::Pin;

use super::*;

#[test]
fn expires_in_deadline_order_without_exceeding_the_batch() {
    let mut timers = TimerQueue::new(4);
    let first = timer(1);
    let second = timer(2);
    let third = timer(3);
    unsafe {
        timers.arm(second, 20).unwrap();
        timers.arm(first, 10).unwrap();
        timers.arm(third, 30).unwrap();
    }
    let mut expired = [ExpiredTimer::EMPTY; 3];

    let result = timers.expire(ExpireRequest::new(30, 2, 5), &mut expired);

    assert_eq!(result.processed(), 2);
    assert_eq!(result.expired(), 2);
    assert!(result.pending());
    assert_eq!(result.next_deadline_ns(), Some(35));
    assert_eq!(expired[0].owner(), 1);
    assert_eq!(expired[1].owner(), 2);
}

#[test]
fn cancellation_uses_a_generation_tombstone() {
    let mut timers = TimerQueue::new(2);
    let node = timer(7);
    let token = unsafe { timers.arm(node, 10).unwrap() };
    assert!(node.cancel(token));
    let replacement = unsafe { timers.arm(node, 20).unwrap() };
    let mut expired = [ExpiredTimer::EMPTY; 1];

    let stale = timers.expire(ExpireRequest::new(10, 1, 1), &mut expired);
    assert_eq!(stale.processed(), 1);
    assert_eq!(stale.expired(), 0);
    assert_eq!(stale.next_deadline_ns(), Some(20));

    let live = timers.expire(ExpireRequest::new(20, 1, 1), &mut expired);
    assert_eq!(live.expired(), 1);
    assert_eq!(expired[0].token(), replacement);
}

#[test]
fn reports_capacity_without_growing_the_heap() {
    let mut timers = TimerQueue::new(1);
    let first = timer(1);
    let second = timer(2);
    unsafe { timers.arm(first, 10).unwrap() };

    assert_eq!(unsafe { timers.arm(second, 20) }, Err(TimerError::Capacity));
    assert_eq!(timers.capacity(), 1);
}

#[test]
fn cancellation_removes_entry_and_reclaims_capacity_immediately() {
    let mut timers = TimerQueue::new(1);
    let first = timer(11);
    let second = timer(22);
    let token = unsafe { timers.arm(first, 10).unwrap() };

    assert!(timers.cancel(first, token));
    assert!(timers.is_empty());
    assert!(unsafe { timers.arm(second, 20) }.is_ok());
}

#[test]
fn cancellation_can_physically_remove_a_rearmed_generation_tombstone() {
    let mut timers = TimerQueue::new(2);
    let node = timer(33);
    let stale = unsafe { timers.arm(node, 10).unwrap() };
    let live = unsafe { timers.arm(node, 20).unwrap() };

    assert!(timers.cancel(node, stale));
    assert_eq!(timers.len(), 1);
    assert!(timers.cancel(node, live));
    assert!(timers.is_empty());
}

fn timer(owner: usize) -> Pin<&'static TimerNode> {
    let node = Box::leak(Box::new(TimerNode::new(owner)));
    unsafe {
        // The leaked node never moves and outlives every queue entry in the test.
        Pin::new_unchecked(node)
    }
}
