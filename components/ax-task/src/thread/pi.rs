//! Priority-inheritance mutex identities and wait handshake tokens.

use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::ThreadId;

/// Stable identity of one kernel PI mutex.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PiLockId(usize);

impl PiLockId {
    /// Creates a PI lock identity from its stable address-sized key.
    pub const fn new(value: usize) -> Self {
        Self(value)
    }

    /// Returns the underlying identity key.
    pub const fn get(self) -> usize {
        self.0
    }
}

/// Token joining ax-sync's waiter grant with ax-task's parking transition.
#[derive(Clone, Debug)]
pub struct PiWaitToken {
    pub(crate) state: Arc<PiWaitState>,
}

impl PiWaitToken {
    /// Returns whether ownership handoff has already selected this waiter.
    pub fn is_granted(&self) -> bool {
        self.state.granted.load(Ordering::Acquire)
    }

    pub(crate) fn waiter(&self) -> ThreadId {
        self.state.waiter
    }
}

#[derive(Debug)]
pub(crate) struct PiWaitState {
    pub(crate) lock: PiLockId,
    pub(crate) waiter: ThreadId,
    owner: AtomicU64,
    pub(crate) granted: AtomicBool,
    pub(crate) cancelled: AtomicBool,
}

impl PiWaitState {
    pub(crate) fn new(lock: PiLockId, waiter: ThreadId, owner: ThreadId, granted: bool) -> Self {
        Self {
            lock,
            waiter,
            owner: AtomicU64::new(owner.as_u64()),
            granted: AtomicBool::new(granted),
            cancelled: AtomicBool::new(false),
        }
    }

    pub(crate) fn owner(&self) -> ThreadId {
        let raw = self.owner.load(Ordering::Acquire);
        ThreadId::from_parts(raw as u32, (raw >> 32) as u32)
    }

    pub(crate) fn set_owner(&self, owner: ThreadId) {
        self.owner.store(owner.as_u64(), Ordering::Release);
    }
}
