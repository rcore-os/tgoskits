#[path = "../src/sync/mutex/entry.rs"]
mod entry;
#[path = "../src/system/task_system/pi/transition.rs"]
mod transition;

use core::{
    cell::Cell,
    sync::atomic::{AtomicU64, Ordering},
};

use entry::{
    FastLockAttempt, FastReleaseAttempt, LockEntry, capture_current_and_prepare_slow,
    owner_spin_eligible, owner_spin_progress_gates, try_release_current_owner_word,
};
use transition::{
    PiOwnerRqAccountingPath, owner_rq_needs_current_settlement, publish_owner_after_waiter_detach,
};

#[test]
fn uncontended_entry_does_not_validate_a_blocking_context() {
    let blocking_validations = Cell::new(0);

    let entry = capture_current_and_prepare_slow(
        || 1_u64,
        |_| FastLockAttempt::Acquired,
        || blocking_validations.set(blocking_validations.get() + 1),
    );

    assert!(matches!(entry, LockEntry::Acquired));
    assert_eq!(blocking_validations.get(), 0);
}

#[test]
fn contended_entry_attempts_owner_fastpath_once_before_slowpath() {
    let fast_attempts = Cell::new(0);
    let current_captures = Cell::new(0);
    let blocking_validations = Cell::new(0);

    let entry = capture_current_and_prepare_slow(
        || {
            current_captures.set(current_captures.get() + 1);
            1_u64
        },
        |_| {
            fast_attempts.set(fast_attempts.get() + 1);
            FastLockAttempt::Contended
        },
        || blocking_validations.set(blocking_validations.get() + 1),
    );

    assert!(matches!(entry, LockEntry::Contended(1)));
    assert_eq!(
        fast_attempts.get(),
        1,
        "Linux rtmutex performs one owner-word fast attempt before the wait-locked slowpath",
    );
    assert_eq!(
        current_captures.get(),
        1,
        "the move-only current token must be reused from fast attempt through slow entry",
    );
    assert_eq!(blocking_validations.get(), 1);
}

#[test]
fn fast_release_rejects_a_different_executing_identity() {
    let owner = AtomicU64::new(7);

    assert_eq!(
        try_release_current_owner_word(&owner, 8, u64::MAX >> 1),
        FastReleaseAttempt::InvalidOwner,
    );
    assert_eq!(owner.load(Ordering::Relaxed), 7);
}

#[test]
fn single_cpu_spin_gate_skips_owner_progress_observations() {
    let progress_observations = Cell::new(0);

    let eligible = owner_spin_eligible(1, || {
        progress_observations.set(progress_observations.get() + 1);
        true
    });

    assert!(!eligible);
    assert_eq!(
        progress_observations.get(),
        0,
        "Linux compiles owner spinning out on non-SMP and performs no owner-progress observations",
    );
}

#[test]
fn owner_spin_requires_every_linux_progress_gate() {
    assert!(owner_spin_eligible(2, || owner_spin_progress_gates(
        true, true, true, false,
    )));
    assert!(!owner_spin_eligible(2, || owner_spin_progress_gates(
        false, true, true, false,
    )));
    assert!(!owner_spin_eligible(2, || owner_spin_progress_gates(
        true, false, true, false,
    )));
    assert!(!owner_spin_eligible(2, || owner_spin_progress_gates(
        true, true, false, false,
    )));
    assert!(!owner_spin_eligible(2, || owner_spin_progress_gates(
        true, true, true, true,
    )));
}

#[test]
fn waiter_edge_is_detached_before_owner_update() {
    let mut events = Vec::new();

    publish_owner_after_waiter_detach(
        &mut events,
        |events| {
            events.push("detach waiter");
            Ok::<_, ()>(())
        },
        |events, _detached| {
            events.push("publish owner");
            Ok::<_, ()>(())
        },
        |_events, _detached| unreachable!("successful publication must not roll back"),
    )
    .unwrap();

    assert_eq!(events, ["detach waiter", "publish owner"]);
}

#[test]
fn failed_owner_update_restores_the_detached_waiter() {
    let mut events = Vec::new();

    let result = publish_owner_after_waiter_detach(
        &mut events,
        |events| {
            events.push("detach waiter");
            Ok::<_, &'static str>(())
        },
        |events, _detached| {
            events.push("publish owner");
            Err::<(), _>("owner update failed")
        },
        |events, _detached| events.push("restore waiter"),
    );

    assert_eq!(result, Err("owner update failed"));
    assert_eq!(events, ["detach waiter", "publish owner", "restore waiter"]);
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TestSchedulerClass {
    Stop,
    Deadline,
    Realtime,
    Fair,
}

#[test]
fn pi_owner_update_accounts_the_linux_class_dequeue_path() {
    assert!(owner_rq_needs_current_settlement(
        PiOwnerRqAccountingPath::Running,
        Some(TestSchedulerClass::Stop),
        Some(TestSchedulerClass::Stop),
    ));

    for class in [
        TestSchedulerClass::Deadline,
        TestSchedulerClass::Realtime,
        TestSchedulerClass::Fair,
    ] {
        assert!(owner_rq_needs_current_settlement(
            PiOwnerRqAccountingPath::QueuedClassDequeue,
            Some(class),
            Some(class),
        ));
    }

    assert!(!owner_rq_needs_current_settlement(
        PiOwnerRqAccountingPath::QueuedClassDequeue,
        None,
        Some(TestSchedulerClass::Stop),
    ));
    assert!(!owner_rq_needs_current_settlement(
        PiOwnerRqAccountingPath::QueuedClassDequeue,
        Some(TestSchedulerClass::Fair),
        Some(TestSchedulerClass::Realtime),
    ));
    assert!(!owner_rq_needs_current_settlement(
        PiOwnerRqAccountingPath::Inactive,
        Some(TestSchedulerClass::Fair),
        Some(TestSchedulerClass::Fair),
    ));
    assert!(!owner_rq_needs_current_settlement(
        PiOwnerRqAccountingPath::QueuedClassDequeue,
        Some(TestSchedulerClass::Fair),
        None,
    ));
}
