//! Shared host-stream and IRQ endpoint for virtual UART register models.

use alloc::sync::Arc;

use ax_sync::{Mutex, MutexGuard};
use axdevice_base::{DeviceError, DeviceResult, IrqLine};

use super::SerialBackend;

pub(super) const RX_POLL_BUFFER_SIZE: usize = 64;

pub(super) struct SerialEndpoint {
    backend: Arc<dyn SerialBackend>,
    irq: IrqLine,
    rx_read: Mutex<()>,
    irq_update: Mutex<()>,
    irq_operation: &'static str,
}

impl SerialEndpoint {
    pub(super) fn new(
        backend: Arc<dyn SerialBackend>,
        irq: IrqLine,
        irq_operation: &'static str,
    ) -> Self {
        Self {
            backend,
            irq,
            rx_read: Mutex::new(()),
            irq_update: Mutex::new(()),
            irq_operation,
        }
    }

    pub(super) fn lock_rx(&self) -> MutexGuard<'_, ()> {
        // A poll may carry the only coalesced work notification. Wait for an
        // earlier read rather than discard the obligation to check the backend.
        self.rx_read.lock()
    }

    pub(super) fn try_lock_rx(&self) -> Option<MutexGuard<'_, ()>> {
        // Register consumption can reenter from a backend callback. Its refill
        // must not wait for that callback's own in-flight read. Avoid even a
        // recursive try_lock: the native lockdep provider rejects that attempt.
        // This observation only skips work; try_lock still establishes ownership
        // if another task acquires the gate after the observation.
        if self.rx_read.is_locked() {
            return None;
        }
        self.rx_read.try_lock()
    }

    /// Reads only the capacity reserved by the UART, then publishes its IRQ.
    pub(super) fn poll_rx(
        &self,
        capacity: usize,
        receive: impl FnOnce(&[u8]),
        irq_level: impl FnOnce() -> bool,
    ) -> DeviceResult {
        let mut completion = RxCompletion(Some(receive));
        let mut bytes = [0; RX_POLL_BUFFER_SIZE];
        let capacity = capacity.min(bytes.len());
        let count = self.backend.read(&mut bytes[..capacity]).min(capacity);
        completion.finish(&bytes[..count]);
        self.refresh_irq(irq_level)
    }

    pub(super) fn try_write(&self, bytes: &[u8]) -> usize {
        self.backend.try_write(bytes).min(bytes.len())
    }

    pub(super) fn refresh_irq(&self, irq_level: impl FnOnce() -> bool) -> DeviceResult {
        // Every caller releases its UART state guard before this gate. Read
        // the current level after acquiring it, so an older MMIO/poll snapshot
        // cannot overwrite a newer transition. The state guard is released
        // again before entering the controller sink. As with IrqLine's own
        // transition gates, the sink must not synchronously reenter this UART.
        let _update = self.irq_update.lock();
        let result = if irq_level() {
            self.irq.assert()
        } else {
            self.irq.deassert()
        };
        result.map_err(|error| DeviceError::Backend {
            operation: self.irq_operation,
            detail: alloc::format!("{error}"),
        })
    }
}

/// Releases the RX reservation if a host backend unwinds before completion.
struct RxCompletion<F: FnOnce(&[u8])>(Option<F>);

impl<F: FnOnce(&[u8])> RxCompletion<F> {
    fn finish(&mut self, bytes: &[u8]) {
        self.0.take().expect("RX completion is consumed once")(bytes)
    }
}

impl<F: FnOnce(&[u8])> Drop for RxCompletion<F> {
    fn drop(&mut self) {
        if let Some(receive) = self.0.take() {
            receive(&[]);
        }
    }
}
