//! Queue-owner links and scoped driver-access ownership.

use core::ops::{Deref, DerefMut};

use ax_kspin::IrqGuard;
use rdif_block::QueueHandle;

use super::{HardwareQueue, HctxAccessPermit};

pub(super) struct DriverAccessGuard<'queue> {
    pub(super) queue: &'queue HardwareQueue,
    pub(super) permit: Option<HctxAccessPermit>,
}

/// Owner-thread lease that moves the portable endpoint out of its short lock.
///
/// Driver callbacks run while this value is live, never while holding the
/// queue's spin/preemption guard. Dropping the lease restores the endpoint to
/// its stable slot before another bounded owner pass can begin.
pub(super) struct DriverEndpointLease<'queue> {
    queue: &'queue HardwareQueue,
    driver: Option<QueueHandle>,
    irq_guard: Option<IrqGuard>,
}

impl<'queue> DriverEndpointLease<'queue> {
    pub(super) fn take(queue: &'queue HardwareQueue) -> Option<Self> {
        // Queue MMIO and the queue's hard-IRQ endpoint belong to the same CPU.
        // Exclude that top half before touching the stable endpoint slot; the
        // portable driver's internal ownership atomics then remain invariant
        // checks rather than a normal Busy/retry path.
        let irq_guard = IrqGuard::new();
        let driver = queue.queue.lock().take()?;
        Some(Self {
            queue,
            driver: Some(driver),
            irq_guard: Some(irq_guard),
        })
    }

    pub(super) fn into_inner(mut self) -> QueueHandle {
        self.driver
            .take()
            .expect("driver endpoint lease consumed twice")
    }
}

impl Deref for DriverEndpointLease<'_> {
    type Target = QueueHandle;

    fn deref(&self) -> &Self::Target {
        self.driver
            .as_ref()
            .expect("driver endpoint lease missing its owner")
    }
}

impl DerefMut for DriverEndpointLease<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.driver
            .as_mut()
            .expect("driver endpoint lease missing its owner")
    }
}

impl Drop for DriverEndpointLease<'_> {
    fn drop(&mut self) {
        if let Some(driver) = self.driver.take() {
            let mut slot = self.queue.queue.lock();
            assert!(slot.is_none(), "block driver endpoint slot was replaced");
            *slot = Some(driver);
            drop(slot);
        }
        // A consumed lease deliberately leaves the slot empty, but it still
        // must end the same local-IRQ exclusion explicitly before returning.
        drop(self.irq_guard.take());
    }
}

impl Drop for DriverAccessGuard<'_> {
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
