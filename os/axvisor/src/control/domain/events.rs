//! VM registry change events for the browser UI (`browser-console` feature).
//!
//! The registry (`axvm`) is the only authoritative VM state. This module reports
//! the differences it observes in it so a browser list can refresh without
//! polling `GET /api/vms`, and each event carries only fields the registry
//! already has (id, name, status) — never a second copy of the control state.
//!
//! A watcher thread compares the registry against its own snapshot every
//! [`POLL_INTERVAL`] and publishes one event per change:
//!
//! - `created`: the VM appeared,
//! - `removed`: the VM disappeared,
//! - `status`: the VM's status changed.
//!
//! Diffing the registry instead of publishing from the create/start/delete call
//! sites means every path that can change a VM is covered, including changes no
//! request caused (a guest that exits on its own, a stop that completes after the
//! request returned). The trade-off is up to [`POLL_INTERVAL`] of latency.
//!
//! Nothing is published while no browser is subscribed, and the watcher drops
//! its snapshot before it goes idle, so a new subscriber is never sent a burst of
//! stale transitions: it starts from the current list (`GET /api/vms`).

use core::sync::atomic::{AtomicUsize, Ordering};
use std::{
    collections::BTreeMap,
    string::String,
    sync::{Mutex, OnceLock},
    thread,
    time::Duration,
};

use axvm::VMId;

/// How long the watcher waits between registry snapshots while subscribed.
const POLL_INTERVAL: Duration = Duration::from_millis(250);

/// How long the idle watcher sleeps before re-checking for subscribers, so a
/// missed wakeup cannot strand it.
const IDLE_PARK: Duration = Duration::from_secs(5);

/// Upper bound on events a slow subscriber may fall behind by. A full queue
/// drops the newest event instead of stalling the watcher; the client resyncs
/// from `GET /api/vms`.
const SUBSCRIBER_QUEUE_CAPACITY: usize = 64;

/// What happened to the VM.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum EventKind {
    Created,
    Removed,
    StatusChanged,
}

impl EventKind {
    /// Stable token the browser client matches on.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Removed => "removed",
            Self::StatusChanged => "status",
        }
    }
}

/// One registry change, with the fields the browser list needs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct VmEvent {
    pub(crate) kind: EventKind,
    pub(crate) id: VMId,
    pub(crate) name: String,
    pub(crate) status: &'static str,
}

/// One browser's event stream. Dropping it unsubscribes.
pub(crate) struct Subscription {
    receiver: tokio::sync::mpsc::Receiver<VmEvent>,
}

impl Subscription {
    /// Waits for the next registry change.
    pub(crate) async fn recv(&mut self) -> Option<VmEvent> {
        self.receiver.recv().await
    }
}

static SUBSCRIBERS: Mutex<Vec<tokio::sync::mpsc::Sender<VmEvent>>> = Mutex::new(Vec::new());
static SUBSCRIBER_COUNT: AtomicUsize = AtomicUsize::new(0);
static WATCHER: OnceLock<thread::Thread> = OnceLock::new();

/// Starts the registry watcher.
///
/// Called once from the boot path before the HTTP listener accepts connections,
/// so the first subscriber cannot miss a change. A failed spawn is not fatal:
/// the control plane keeps working and the UI can still read `GET /api/vms`.
pub(crate) fn start() {
    match thread::Builder::new()
        .name("axvisor-vm-events".into())
        .spawn(run_watcher)
    {
        Ok(handle) => {
            let _ = WATCHER.set(handle.thread().clone());
        }
        Err(error) => warn!("VM event watcher unavailable: {error}"),
    }
}

/// Registers one browser's event stream and wakes the watcher.
pub(crate) fn subscribe() -> Subscription {
    let (sender, receiver) = tokio::sync::mpsc::channel(SUBSCRIBER_QUEUE_CAPACITY);
    match SUBSCRIBERS.lock() {
        Ok(mut subscribers) => {
            subscribers.push(sender);
            SUBSCRIBER_COUNT.store(subscribers.len(), Ordering::Release);
        }
        Err(_) => error!("VM event subscriber list is poisoned; this browser gets no events"),
    }
    if let Some(watcher) = WATCHER.get() {
        watcher.unpark();
    }
    Subscription { receiver }
}

/// A VM's name and status, as read from the registry.
type VmSnapshot = BTreeMap<VMId, (String, &'static str)>;

fn run_watcher() {
    let mut baseline = VmSnapshot::new();
    // Set when the watcher resumes with subscribers after an idle period: the
    // snapshot it holds is then stale enough to be misleading, so the first
    // subscriber starts from the list it reads itself.
    let mut resync = true;
    loop {
        if SUBSCRIBER_COUNT.load(Ordering::Acquire) == 0 {
            thread::park_timeout(IDLE_PARK);
            resync = true;
            continue;
        }
        let current = registry_snapshot();
        if !resync {
            publish_diff(&baseline, &current);
        }
        resync = false;
        baseline = current;
        thread::sleep(POLL_INTERVAL);
    }
}

fn registry_snapshot() -> VmSnapshot {
    crate::manager::AxvmManager::vm_list()
        .iter()
        .map(|vm| (vm.id(), (vm.name(), vm.status().as_str())))
        .collect()
}

fn publish_diff(baseline: &VmSnapshot, current: &VmSnapshot) {
    let mut events = Vec::new();
    for (id, (name, status)) in current {
        match baseline.get(id) {
            None => events.push(VmEvent {
                kind: EventKind::Created,
                id: *id,
                name: name.clone(),
                status,
            }),
            Some((_, previous)) if previous != status => events.push(VmEvent {
                kind: EventKind::StatusChanged,
                id: *id,
                name: name.clone(),
                status,
            }),
            Some(_) => {}
        }
    }
    for (id, (name, status)) in baseline {
        if !current.contains_key(id) {
            events.push(VmEvent {
                kind: EventKind::Removed,
                id: *id,
                name: name.clone(),
                status,
            });
        }
    }
    if !events.is_empty() {
        publish(&events);
    }
}

fn publish(events: &[VmEvent]) {
    let Ok(mut subscribers) = SUBSCRIBERS.lock() else {
        error!(
            "VM event subscriber list is poisoned; dropping {} event(s)",
            events.len()
        );
        return;
    };
    subscribers.retain(|sender| {
        for event in events {
            match sender.try_send(event.clone()) {
                Ok(()) => {}
                // The browser is gone: stop tracking it.
                Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => return false,
                // The browser is behind: drop this event, keep the stream.
                Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {}
            }
        }
        true
    });
    SUBSCRIBER_COUNT.store(subscribers.len(), Ordering::Release);
}
