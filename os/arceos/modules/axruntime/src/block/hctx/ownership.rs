//! Queue-owner links and scoped driver-access ownership.

use core::{ptr, sync::atomic::AtomicPtr};

use super::{HardwareQueue, HctxAccessPermit};

pub(super) struct WorkOwnerLink {
    pub(super) owner: AtomicPtr<HardwareQueue>,
}

pub(super) struct DriverAccessGuard {
    pub(super) queue: &'static HardwareQueue,
    pub(super) permit: Option<HctxAccessPermit>,
}

impl Drop for DriverAccessGuard {
    fn drop(&mut self) {
        let permit = self
            .permit
            .take()
            .expect("hctx driver access guard released twice");
        if self.queue.access_gate.leave(permit) {
            self.queue.controller_link.wake_recovery();
        }
    }
}

impl WorkOwnerLink {
    pub(super) const fn new() -> Self {
        Self {
            owner: AtomicPtr::new(ptr::null_mut()),
        }
    }
}
