use alloc::boxed::Box;

use super::*;

#[test]
fn expires_in_deadline_order_without_exceeding_the_batch() {
    let first = timer(1);
    let second = timer(2);
    let third = timer(3);
    let mut timers = TaskDeadlineQueue::new(4);
    let _second_registration = timers.arm(second.as_ref(), 20, park(2)).unwrap();
    let _first_registration = timers.arm(first.as_ref(), 10, park(1)).unwrap();
    let _third_registration = timers.arm(third.as_ref(), 30, park(3)).unwrap();
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
    let _first_registration = timers.arm(first.as_ref(), 10, park(1)).unwrap();

    assert_eq!(
        timers.arm(second.as_ref(), 20, park(2)),
        Err(TaskDeadlineError::Capacity)
    );
    assert_eq!(timers.capacity(), 1);
}

#[test]
fn no_deadline_sentinel_cannot_consume_queue_capacity() {
    let first = timer(3);
    let second = timer(4);
    let mut timers = TaskDeadlineQueue::new(1);

    assert!(
        timers.arm(first.as_ref(), u64::MAX, park(1)).is_err(),
        "u64::MAX represents no finite deadline and must not enter the heap"
    );
    assert!(timers.is_empty());

    let live = timers.arm(second.as_ref(), 10, park(2)).unwrap();
    assert!(timers.cancel(&live));
    assert!(timers.is_empty());
}

#[test]
fn zero_is_an_immediately_due_logical_deadline() {
    let node = timer(5);
    let mut timers = TaskDeadlineQueue::new(1);
    let registration = timers.arm(node.as_ref(), 0, park(1)).unwrap();
    let mut expired = [ExpiredTaskDeadline::EMPTY; 1];

    let batch = timers.expire(TaskDeadlineExpireRequest::new(0, 1, 1), &mut expired);

    assert_eq!(batch.processed(), 1);
    assert_eq!(batch.expired(), 1);
    assert_eq!(expired[0].token(), registration.token());
    assert!(timers.is_empty());
}

#[test]
fn rearm_replaces_the_existing_entry_without_consuming_capacity() {
    let node = timer(7);
    let mut timers = TaskDeadlineQueue::new(1);
    let first = timers.arm(node.as_ref(), 10, park(1)).unwrap();

    let second = timers.arm(node.as_ref(), 20, park(1)).unwrap();

    assert_ne!(first.token(), second.token());
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
    assert_eq!(expired[0].token(), second.token());
}

#[test]
fn one_thread_can_own_independent_park_cbs_and_zero_lag_entries() {
    let park_node = timer(8);
    let cbs_node = Box::new(TaskDeadlineNode::deadline_cbs_for_thread(thread(8)));
    let zero_lag_node = Box::new(TaskDeadlineNode::deadline_zero_lag_for_thread(thread(8)));
    let mut timers = TaskDeadlineQueue::new(1);

    let _park = timers.arm(park_node.as_ref(), 30, park(1)).unwrap();
    let _cbs = timers
        .arm(cbs_node.as_ref(), 10, TaskDeadlineKind::DeadlineCbs)
        .unwrap();
    let _zero_lag = timers
        .arm(
            zero_lag_node.as_ref(),
            20,
            TaskDeadlineKind::DeadlineZeroLag,
        )
        .unwrap();

    assert_eq!(timers.capacity(), 1);
    assert_eq!(timers.len(), 3);

    let mut expired = [ExpiredTaskDeadline::EMPTY; 3];
    let batch = timers.expire(TaskDeadlineExpireRequest::new(30, 3, 1), &mut expired);
    assert_eq!(batch.expired(), 3);
    assert_eq!(expired[0].kind(), Some(TaskDeadlineKind::DeadlineCbs));
    assert_eq!(expired[1].kind(), Some(TaskDeadlineKind::DeadlineZeroLag));
    assert_eq!(expired[2].kind(), Some(park(1)));
}

#[test]
fn rearm_replaces_only_the_matching_typed_slot() {
    let park_node = timer(9);
    let cbs_node = Box::new(TaskDeadlineNode::deadline_cbs_for_thread(thread(9)));
    let mut timers = TaskDeadlineQueue::new(1);
    let park = timers.arm(park_node.as_ref(), 10, park(1)).unwrap();
    let stale_cbs = timers
        .arm(cbs_node.as_ref(), 20, TaskDeadlineKind::DeadlineCbs)
        .unwrap();

    let live_cbs = timers
        .arm(cbs_node.as_ref(), 30, TaskDeadlineKind::DeadlineCbs)
        .unwrap();

    assert_eq!(timers.len(), 2);
    assert!(!timers.cancel(&stale_cbs));
    assert!(timers.cancel(&park));
    assert!(timers.cancel(&live_cbs));
    assert!(timers.is_empty());
}

#[test]
fn node_rejects_a_deadline_kind_from_another_slot() {
    let node = timer(10);
    let mut timers = TaskDeadlineQueue::new(1);

    assert_eq!(
        timers.arm(node.as_ref(), 10, TaskDeadlineKind::DeadlineCbs),
        Err(TaskDeadlineError::KindMismatch)
    );
    assert!(timers.is_empty());
}

#[test]
fn cancellation_removes_entry_and_reclaims_capacity_immediately() {
    let first = timer(11);
    let second = timer(22);
    let mut timers = TaskDeadlineQueue::new(1);
    let registration = timers.arm(first.as_ref(), 10, park(1)).unwrap();

    assert!(timers.cancel(&registration));
    assert!(timers.is_empty());
    assert!(timers.arm(second.as_ref(), 20, park(2)).is_ok());
}

#[test]
fn cancellation_transaction_restores_the_exact_registration_and_capacity() {
    let first = timer(12);
    let second = timer(24);
    let mut timers = TaskDeadlineQueue::new(1);
    let registration = timers.arm(first.as_ref(), 10, park(1)).unwrap();
    let token = registration.token();

    let cancellation = timers
        .begin_cancel(&registration)
        .expect("the live registration must begin a cancellation transaction");
    assert!(timers.is_empty());
    cancellation.rollback(&mut timers);
    assert_eq!(timers.len(), 1);
    assert_eq!(timers.next_deadline_ns(0, 1), Some(10));
    assert_eq!(
        timers.arm(second.as_ref(), 20, park(2)),
        Err(TaskDeadlineError::Capacity),
        "rollback must restore the cancelled entry's class capacity"
    );

    let mut expired = [ExpiredTaskDeadline::EMPTY; 1];
    assert_eq!(
        timers
            .expire(TaskDeadlineExpireRequest::new(10, 1, 1), &mut expired)
            .expired(),
        1
    );
    assert_eq!(
        expired[0].token(),
        token,
        "rollback must not manufacture a new timer generation"
    );
    assert!(timers.is_empty());
}

#[test]
fn cancellation_rejects_the_non_arm_token() {
    let node = timer(23);
    let mut timers = TaskDeadlineQueue::new(1);

    assert!(
        !timers.cancel(&TaskDeadlineRegistration::new(
            thread(23),
            TaskDeadlineToken::NONE,
            0,
            park(1),
        )),
        "the NONE sentinel must not identify an active arm operation"
    );
    assert!(timers.is_empty());

    let live = timers.arm(node.as_ref(), 10, park(1)).unwrap();
    assert!(timers.cancel(&live));
    assert!(!timers.cancel(&TaskDeadlineRegistration::new(
        thread(23),
        TaskDeadlineToken::NONE,
        0,
        park(1),
    )));
}

#[test]
fn stale_generation_cancel_cannot_remove_the_rearmed_entry() {
    let node = timer(33);
    let mut timers = TaskDeadlineQueue::new(1);
    let stale = timers.arm(node.as_ref(), 10, park(1)).unwrap();
    let live = timers.arm(node.as_ref(), 20, park(1)).unwrap();

    assert!(!timers.cancel(&stale));
    assert_eq!(timers.len(), 1);
    assert!(timers.cancel(&live));
    assert!(timers.is_empty());
}

#[test]
fn distinct_nodes_for_one_thread_keep_independent_registration_identity() {
    let first_node = timer(34);
    let second_node = timer(34);
    let mut timers = TaskDeadlineQueue::new(2);
    let first = timers.arm(first_node.as_ref(), 10, park(1)).unwrap();
    let second = timers.arm(second_node.as_ref(), 20, park(1)).unwrap();

    assert_eq!(
        timers.len(),
        2,
        "each physical timer node must retain its own queue entry"
    );
    assert!(timers.cancel(&first));
    assert_eq!(timers.len(), 1);
    assert!(timers.cancel(&second));
    assert!(timers.is_empty());
}

#[test]
fn queued_deadline_owns_expiry_identity_by_value() {
    let node = timer(44);
    let mut timers = TaskDeadlineQueue::new(1);
    let registration = timers.arm(node.as_ref(), 10, park(9)).unwrap();
    let token = registration.token();

    drop(node);

    let mut expired = [ExpiredTaskDeadline::EMPTY; 1];
    let batch = timers.expire(TaskDeadlineExpireRequest::new(10, 1, 1), &mut expired);
    assert_eq!(batch.expired(), 1);
    assert_eq!(expired[0].thread(), Some(thread(44)));
    assert_eq!(expired[0].token(), token);
}

fn timer(slot: u32) -> Box<TaskDeadlineNode> {
    Box::new(TaskDeadlineNode::for_thread(thread(slot)))
}

const fn park(generation: u64) -> TaskDeadlineKind {
    TaskDeadlineKind::park_timeout(generation)
}

const fn thread(slot: u32) -> crate::ThreadId {
    crate::ThreadId::from_parts(slot, 1)
}
