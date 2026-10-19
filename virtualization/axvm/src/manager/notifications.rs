//! Coalesced lifecycle invalidations; snapshots remain the only state authority.

use std::{
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, Ordering},
    },
    task::Waker,
};

use super::MutexExt;

/// A task-context subscription to one manager's lifecycle changes.
///
/// Notifications are coalesced invalidations, not a queue of old VM states.
/// After consuming an invalidation, read the manager's current handles and
/// snapshots. Their `VmKey` and `RunId` identify the current incarnation.
/// Dropping this subscription cancels it; an already selected waker may finish.
pub struct VmLifecycleSubscription {
    observer: Arc<Observer>,
}

impl VmLifecycleSubscription {
    /// Reports whether initial reconciliation or a lifecycle update is pending.
    pub fn has_changed(&self) -> bool {
        self.observer.pending.load(Ordering::Acquire)
    }

    /// Consumes the pending invalidation before reading authoritative state.
    ///
    /// A concurrent publication remains pending for the next reconciliation.
    /// This subscription must have one logical consumer; its notifier never
    /// consumes the flag.
    pub fn take_changed(&mut self) -> bool {
        self.observer.pending.swap(false, Ordering::AcqRel)
    }
}

struct Observer {
    pending: AtomicBool,
    waker: Waker,
}

#[derive(Default)]
pub(super) struct LifecycleEvents {
    observers: Mutex<Vec<Weak<Observer>>>,
}

impl LifecycleEvents {
    pub(super) fn subscribe(&self, waker: Waker) -> VmLifecycleSubscription {
        let observer = Arc::new(Observer {
            // Registration starts dirty, so state published before subscribing
            // is reconciled without requiring a second publication.
            pending: AtomicBool::new(true),
            waker,
        });
        let mut observers = self.observers.lock_unpoisoned();
        observers.retain(|observer| observer.strong_count() != 0);
        observers.push(Arc::downgrade(&observer));
        VmLifecycleSubscription { observer }
    }

    pub(super) fn notify(&self) {
        let observers = {
            let mut registered = self.observers.lock_unpoisoned();
            let mut observers = Vec::new();
            registered.retain(|observer| {
                if let Some(observer) = observer.upgrade() {
                    observers.push(observer);
                    true
                } else {
                    false
                }
            });
            observers
        };
        // Never invoke external wakers under registry, snapshot or subscription
        // locks. Repeated publications only need one outstanding invalidation.
        for observer in observers {
            if !observer.pending.swap(true, Ordering::AcqRel) {
                observer.waker.wake_by_ref();
            }
        }
    }
}
