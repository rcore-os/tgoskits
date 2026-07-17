use alloc::boxed::Box;
use core::pin::Pin;

use super::*;
use crate::ThreadId;

#[test]
fn expires_in_deadline_order_without_exceeding_the_batch() {
    let first = timer(1);
    let second = timer(2);
    let third = timer(3);
    let mut timers = TimerQueue::new(4);
    unsafe {
        timers.arm(second.as_ref(), 20).unwrap();
        timers.arm(first.as_ref(), 10).unwrap();
        timers.arm(third.as_ref(), 30).unwrap();
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
    let node = timer(7);
    let mut timers = TimerQueue::new(2);
    let token = unsafe { timers.arm(node.as_ref(), 10).unwrap() };
    assert!(node.as_ref().cancel(token));
    let replacement = unsafe { timers.arm(node.as_ref(), 20).unwrap() };
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
    let first = timer(1);
    let second = timer(2);
    let mut timers = TimerQueue::new(1);
    unsafe { timers.arm(first.as_ref(), 10).unwrap() };

    assert_eq!(
        unsafe { timers.arm(second.as_ref(), 20) },
        Err(TimerError::Capacity)
    );
    assert_eq!(timers.capacity(), 1);
}

#[test]
fn cancellation_removes_entry_and_reclaims_capacity_immediately() {
    let first = timer(11);
    let second = timer(22);
    let mut timers = TimerQueue::new(1);
    let token = unsafe { timers.arm(first.as_ref(), 10).unwrap() };

    assert!(timers.cancel(first.as_ref(), token));
    assert!(timers.is_empty());
    assert!(unsafe { timers.arm(second.as_ref(), 20) }.is_ok());
}

#[test]
fn cancellation_can_physically_remove_a_rearmed_generation_tombstone() {
    let node = timer(33);
    let mut timers = TimerQueue::new(2);
    let stale = unsafe { timers.arm(node.as_ref(), 10).unwrap() };
    let live = unsafe { timers.arm(node.as_ref(), 20).unwrap() };

    assert!(timers.cancel(node.as_ref(), stale));
    assert_eq!(timers.len(), 1);
    assert!(timers.cancel(node.as_ref(), live));
    assert!(timers.is_empty());
}

#[test]
fn runtime_timer_rejects_a_scheduler_sleep_node() {
    let node = Box::pin(TimerNode::for_thread(ThreadId::from_parts(7, 1)));
    let owner = unsafe {
        // SAFETY: the opaque scalar is never dereferenced by this queue test.
        RuntimeTimerOwner::new(0x1000, 1)
    };
    let mut timers = TimerQueue::new(1);

    assert_eq!(
        unsafe { timers.arm_runtime(node.as_ref(), 10, owner) },
        Err(TimerError::InvalidOwner)
    );
    assert!(timers.is_empty());
}

fn timer(owner: usize) -> Pin<Box<TimerNode>> {
    Box::pin(TimerNode::new(owner))
}
