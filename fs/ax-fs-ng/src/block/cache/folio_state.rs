//! Folio ownership and a frame budget shared by resident and in-flight data.

use alloc::sync::Arc;
use core::sync::atomic::{AtomicUsize, Ordering};

use super::folio::CacheFolio;
use crate::os::{sync::SleepMutex, waiters::TaskWaiters};

pub(super) struct FrameBudget {
    used: AtomicUsize,
    capacity: usize,
    pub(super) waiters: TaskWaiters,
}

impl FrameBudget {
    pub(super) fn new(capacity: usize) -> Arc<Self> {
        Arc::new(Self {
            used: AtomicUsize::new(0),
            capacity,
            waiters: TaskWaiters::new(),
        })
    }

    pub(super) fn available(&self) -> bool {
        self.used.load(Ordering::Acquire) < self.capacity
    }

    pub(super) fn acquire(self: &Arc<Self>) -> Option<FramePermit> {
        self.used
            .try_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                used.checked_add(1).filter(|next| *next <= self.capacity)
            })
            .ok()?;
        Some(FramePermit(Arc::clone(self)))
    }

    #[cfg(test)]
    pub(super) fn used(&self) -> usize {
        self.used.load(Ordering::Acquire)
    }
}

pub(super) struct FramePermit(Arc<FrameBudget>);

impl Drop for FramePermit {
    fn drop(&mut self) {
        // Release follows destruction of the frame bytes by field order.
        self.0.used.fetch_sub(1, Ordering::AcqRel);
        self.0.waiters.notify_all();
    }
}

pub(super) struct FolioEntry {
    // Lock order: range stripes, then io, then data. Index exclusion is never
    // held while waiting for these locks or issuing device I/O.
    pub(super) io: SleepMutex<()>,
    pub(super) data: SleepMutex<CacheFolio>,
    _permit: FramePermit,
}

impl FolioEntry {
    pub(super) fn new(data: CacheFolio, permit: FramePermit) -> Self {
        Self {
            io: SleepMutex::new(()),
            data: SleepMutex::new(data),
            _permit: permit,
        }
    }
}

pub(super) struct FolioPin<'a> {
    pub(super) entry: Arc<FolioEntry>,
    _wake: PinWake<'a>,
}

impl<'a> FolioPin<'a> {
    pub(super) fn new(entry: Arc<FolioEntry>, budget: &'a FrameBudget) -> Self {
        Self {
            entry,
            _wake: PinWake(budget),
        }
    }
}

struct PinWake<'a>(&'a FrameBudget);

impl Drop for PinWake<'_> {
    fn drop(&mut self) {
        // Struct fields drop in declaration order: entry releases its pin
        // before this notification closes the capacity-to-sleep race.
        self.0.waiters.notify_all();
    }
}
