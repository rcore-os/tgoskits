//! Tests for the typed kernel timer queue.

use alloc::{boxed::Box, sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicUsize, Ordering};

use super::*;
use crate::sync::SpinLock;

fn deadline(nanos: u64) -> MonotonicDeadline {
    MonotonicDeadline::from_nanos(nanos).unwrap()
}

fn instant(nanos: u64) -> MonotonicInstant {
    MonotonicInstant::from_nanos(nanos).unwrap()
}

#[test]
fn restartable_timer_reuses_identity_until_cancelled() {
    let invocations = Arc::new(AtomicUsize::new(0));
    let callback_invocations = Arc::clone(&invocations);
    let entry = KernelTimerEntry::new_restartable(
        deadline(10),
        Box::new(move |_| {
            let invocation = callback_invocations.fetch_add(1, Ordering::Relaxed) + 1;
            KernelTimerAction::Rearm(deadline(10 + invocation as u64 * 10))
        }),
    )
    .unwrap();
    let mut queue = KernelTimerQueue::new(1);
    let handle = queue.insert(TimerCpuId::new(0), entry).unwrap();

    assert_eq!(queue.expire_due_soft(instant(10), 1).expired(), 1);
    let mut execution = queue.claim_expired().unwrap();
    let action = execution.invoke_soft();
    assert!(queue.complete_soft_execution(execution, action).is_none());
    assert_eq!(queue.next_soft_deadline(), Some(deadline(20)));

    assert_eq!(queue.expire_due_soft(instant(20), 1).expired(), 1);
    let mut execution = queue.claim_expired().unwrap();
    let action = execution.invoke_soft();
    assert!(queue.complete_soft_execution(execution, action).is_none());
    assert_eq!(queue.next_soft_deadline(), Some(deadline(30)));
    assert_eq!(invocations.load(Ordering::Relaxed), 2);

    assert!(matches!(
        queue.cancel(handle),
        KernelTimerQueueCancel::Cancelled(_)
    ));
    assert!(!queue.has_active_work());
}

#[test]
fn cancellation_during_callback_prevents_restart() {
    let entry = KernelTimerEntry::new_restartable(
        deadline(10),
        Box::new(|_| KernelTimerAction::Rearm(deadline(20))),
    )
    .unwrap();
    let mut queue = KernelTimerQueue::new(1);
    let handle = queue.insert(TimerCpuId::new(0), entry).unwrap();
    assert_eq!(queue.expire_due_soft(instant(10), 1).expired(), 1);
    let mut execution = queue.claim_expired().unwrap();

    assert!(matches!(
        queue.cancel(handle),
        KernelTimerQueueCancel::Executing
    ));
    let action = execution.invoke_soft();
    assert!(queue.complete_soft_execution(execution, action).is_some());
    assert!(!queue.has_active_work());
}

#[test]
fn hard_completion_defers_callback_reclamation_to_task_context() {
    let invocations = Arc::new(AtomicUsize::new(0));
    let callback_invocations = Arc::clone(&invocations);
    let callback = unsafe {
        // SAFETY: this test callback performs one bounded atomic operation.
        HardKernelTimerCallback::new(Box::new(move |_| {
            callback_invocations.fetch_add(1, Ordering::Relaxed);
            HardKernelTimerAction::Complete
        }))
    };
    let entry = KernelTimerEntry::new_hard_restartable(deadline(10), callback).unwrap();
    let mut queue = KernelTimerQueue::new(1);
    let handle = queue.insert(TimerCpuId::new(0), entry).unwrap();

    let mut execution = queue.claim_due_hard(instant(10)).unwrap();
    let action = unsafe {
        // SAFETY: the pure queue test models the hard callback transaction.
        execution.invoke_hard()
    };
    assert!(queue.complete_hard_execution(execution, action));
    assert_eq!(invocations.load(Ordering::Relaxed), 1);
    assert!(queue.has_completed());
    assert!(matches!(
        queue.cancel(handle),
        KernelTimerQueueCancel::Stale
    ));

    drop(queue.claim_completed());
    assert!(!queue.has_active_work());
}

#[test]
fn hard_disarm_retains_one_stable_registration_without_reaping() {
    let callback = unsafe {
        // SAFETY: this callback returns one constant action.
        HardKernelTimerCallback::new(Box::new(|_| HardKernelTimerAction::Disarm))
    };
    let entry = KernelTimerEntry::new_hard_restartable(deadline(10), callback).unwrap();
    let mut queue = KernelTimerQueue::new(1);
    let handle = queue.insert(TimerCpuId::new(0), entry).unwrap();

    let mut execution = queue.claim_due_hard(instant(10)).unwrap();
    let action = unsafe {
        // SAFETY: the pure queue test models the hard callback transaction.
        execution.invoke_hard()
    };
    assert!(!queue.complete_hard_execution(execution, action));
    assert!(queue.has_inactive());
    assert!(!queue.has_completed());

    assert!(queue.arm_hard(handle, deadline(20)));
    let mut execution = queue.claim_due_hard(instant(20)).unwrap();
    let action = unsafe {
        // SAFETY: the pure queue test models the second hard transaction.
        execution.invoke_hard()
    };
    assert!(!queue.complete_hard_execution(execution, action));
    assert!(queue.has_inactive());
    assert!(matches!(
        queue.cancel(handle),
        KernelTimerQueueCancel::Cancelled(_)
    ));
    assert!(!queue.has_active_work());
}

#[test]
fn task_arm_while_hard_callback_runs_owns_the_next_deadline() {
    let callback = unsafe {
        // SAFETY: this callback returns one constant action.
        HardKernelTimerCallback::new(Box::new(|_| HardKernelTimerAction::Disarm))
    };
    let entry = KernelTimerEntry::new_hard_restartable(deadline(10), callback).unwrap();
    let mut queue = KernelTimerQueue::new(1);
    let handle = queue.insert(TimerCpuId::new(0), entry).unwrap();

    let mut execution = queue.claim_due_hard(instant(10)).unwrap();
    assert!(queue.arm_hard(handle, deadline(20)));
    let action = unsafe {
        // SAFETY: the pure queue test models the hard callback transaction.
        execution.invoke_hard()
    };
    assert!(!queue.complete_hard_execution(execution, action));
    assert_eq!(queue.next_hard_deadline(), Some(deadline(20)));
}

#[test]
fn equal_deadlines_execute_in_registration_order() {
    let order = Arc::new(SpinLock::new(Vec::new()));
    let mut queue = KernelTimerQueue::new(3);
    for sequence in 0..3 {
        let callback_order = Arc::clone(&order);
        let entry = KernelTimerEntry::new(
            deadline(10),
            Box::new(move |_| callback_order.lock().push(sequence)),
        )
        .unwrap();
        queue.insert(TimerCpuId::new(0), entry).unwrap();
    }

    assert_eq!(queue.expire_due_soft(instant(10), 3).expired(), 3);
    for _ in 0..3 {
        let mut execution = queue.claim_expired().unwrap();
        let action = execution.invoke_soft();
        drop(queue.complete_soft_execution(execution, action));
    }
    assert_eq!(&*order.lock(), &[0, 1, 2]);
}

#[test]
fn expiration_budget_reports_due_work_left_behind() {
    let mut queue = KernelTimerQueue::new(3);
    for _ in 0..3 {
        let entry = KernelTimerEntry::new(deadline(10), Box::new(|_| {})).unwrap();
        queue.insert(TimerCpuId::new(0), entry).unwrap();
    }

    let batch = queue.expire_due_soft(instant(10), 2);
    assert_eq!(batch.expired(), 2);
    assert!(batch.pending());
    assert_eq!(queue.next_soft_deadline(), Some(deadline(10)));
}

#[test]
fn cancelling_the_head_advances_to_the_next_deadline() {
    let mut queue = KernelTimerQueue::new(2);
    let later = KernelTimerEntry::new(deadline(30), Box::new(|_| {})).unwrap();
    let earlier = KernelTimerEntry::new(deadline(10), Box::new(|_| {})).unwrap();
    queue.insert(TimerCpuId::new(0), later).unwrap();
    let earlier_handle = queue.insert(TimerCpuId::new(0), earlier).unwrap();

    assert_eq!(queue.next_soft_deadline(), Some(deadline(10)));
    assert!(matches!(
        queue.cancel(earlier_handle),
        KernelTimerQueueCancel::Cancelled(_)
    ));
    assert_eq!(queue.next_soft_deadline(), Some(deadline(30)));
    assert!(matches!(
        queue.cancel(earlier_handle),
        KernelTimerQueueCancel::Stale
    ));
}

#[test]
fn an_early_hard_edge_does_not_claim_the_registration() {
    let callback = unsafe {
        // SAFETY: this callback returns one constant action.
        HardKernelTimerCallback::new(Box::new(|_| HardKernelTimerAction::Disarm))
    };
    let entry = KernelTimerEntry::new_hard_restartable(deadline(20), callback).unwrap();
    let mut queue = KernelTimerQueue::new(1);
    queue.insert(TimerCpuId::new(0), entry).unwrap();

    assert!(queue.claim_due_hard(instant(19)).is_none());
    assert_eq!(queue.next_hard_deadline(), Some(deadline(20)));
    assert!(queue.claim_due_hard(instant(20)).is_some());
}
