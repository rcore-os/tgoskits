//! Concurrency contract for the runtime-owned exit-to-user notification.

use std::sync::Arc as StdArc;

use ax_runtime::task::{UserEntryAck, UserEntryNotification};
use loom::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
};

#[test]
fn concrete_ticket_acknowledges_only_its_snapshot() {
    let notification = StdArc::new(UserEntryNotification::new());
    notification.publish();
    let snapshot = notification.snapshot();

    let producer = StdArc::clone(&notification);
    std::thread::spawn(move || producer.publish())
        .join()
        .unwrap();

    assert!(notification.changed_since(&snapshot));
    assert_eq!(notification.acknowledge(snapshot), UserEntryAck::Pending);
    assert!(notification.pending());

    let newest = notification.snapshot();
    assert_eq!(notification.acknowledge(newest), UserEntryAck::Stable);
    assert!(!notification.pending());
}

#[test]
fn concrete_stale_ack_cannot_regress_a_newer_ack() {
    let notification = UserEntryNotification::new();
    notification.publish();
    let stale = notification.snapshot();
    notification.publish();
    let newest = notification.snapshot();

    assert_eq!(notification.acknowledge(newest), UserEntryAck::Stable);
    assert_eq!(notification.acknowledge(stale), UserEntryAck::Stable);
    assert!(!notification.pending());
}

#[test]
#[should_panic(expected = "ticket belongs to another notification")]
fn concrete_ticket_is_bound_to_its_notification() {
    let first = UserEntryNotification::new();
    let second = UserEntryNotification::new();
    let ticket = first.snapshot();

    let _outcome = second.acknowledge(ticket);
}

#[test]
fn producer_racing_snapshot_ack_always_remains_pending() {
    loom::model(|| {
        let notification = Arc::new(NotificationModel::default());
        notification.publish();
        let snapshot = notification.snapshot();

        let producer_notification = Arc::clone(&notification);
        let producer = thread::spawn(move || producer_notification.publish());
        notification.acknowledge(snapshot);
        producer.join().unwrap();

        assert!(notification.pending());
    });
}

#[test]
fn final_gate_observes_work_or_leaves_the_release_doorbell_pending() {
    loom::model(|| {
        let notification = Arc::new(NotificationModel::default());
        let producer_notification = Arc::clone(&notification);
        let producer = thread::spawn(move || producer_notification.publish());

        let decision = notification.final_irqoff_check();
        producer.join().unwrap();

        match decision {
            GateDecision::Deferred => {}
            GateDecision::Ready => {
                assert!(notification.doorbell.load(Ordering::Acquire));
                assert!(notification.pending());
            }
        }
    });
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GateDecision {
    Deferred,
    Ready,
}

#[derive(Default)]
struct NotificationModel {
    produced: AtomicU64,
    acknowledged: AtomicU64,
    doorbell: AtomicBool,
}

impl NotificationModel {
    fn publish(&self) {
        self.produced.fetch_add(1, Ordering::Release);
        self.doorbell.store(true, Ordering::Release);
    }

    fn snapshot(&self) -> u64 {
        self.produced.load(Ordering::Acquire)
    }

    fn acknowledge(&self, epoch: u64) {
        let mut acknowledged = self.acknowledged.load(Ordering::Acquire);
        while acknowledged < epoch {
            match self.acknowledged.compare_exchange_weak(
                acknowledged,
                epoch,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(actual) => acknowledged = actual,
            }
        }
    }

    fn pending(&self) -> bool {
        let acknowledged = self.acknowledged.load(Ordering::Acquire);
        self.produced.load(Ordering::Acquire) != acknowledged
    }

    fn final_irqoff_check(&self) -> GateDecision {
        if self.pending() {
            GateDecision::Deferred
        } else {
            GateDecision::Ready
        }
    }
}
