//! Shared host-stream and IRQ endpoint for virtual UART register models.

use alloc::sync::Arc;

use axdevice_base::{DeviceError, DeviceResult, IrqLine};

use super::SerialBackend;

const RX_POLL_BUFFER_SIZE: usize = 64;

pub(super) struct SerialEndpoint {
    backend: Arc<dyn SerialBackend>,
    irq: IrqLine,
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
            irq_operation,
        }
    }

    /// Polls the non-blocking backend and publishes the resulting IRQ level.
    pub(super) fn poll_rx(&self, receive: impl FnOnce(&[u8]) -> bool) -> DeviceResult {
        let mut bytes = [0; RX_POLL_BUFFER_SIZE];
        let count = self.backend.read(&mut bytes).min(bytes.len());
        self.set_irq_level(receive(&bytes[..count]))
    }

    pub(super) fn write(&self, bytes: &[u8]) {
        self.backend.write(bytes);
    }

    pub(super) fn set_irq_level(&self, asserted: bool) -> DeviceResult {
        let result = if asserted {
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
