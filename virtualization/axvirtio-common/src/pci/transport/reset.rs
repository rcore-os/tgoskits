use axdevice_base::{DeviceError, DeviceResult};

use super::{VirtioDeviceCore, VirtioPciTransport};
use crate::{constants::VIRTIO_STATUS_DEVICE_NEEDS_RESET, pci::InterruptTransition};

impl<D: VirtioDeviceCore> VirtioPciTransport<D> {
    /// Resets the transport and device-specific state.
    pub fn reset(&self) -> DeviceResult<InterruptTransition> {
        if !self.activity.begin_reset() {
            return Err(DeviceError::InvalidState {
                operation: "reset VirtIO PCI transport",
                detail: "another reset is already in progress".into(),
            });
        }
        {
            let mut state = self.state.lock();
            state.status |= VIRTIO_STATUS_DEVICE_NEEDS_RESET as u8;
            state.device_needs_reset = true;
            state.reset_pending = false;
        }
        if !self.activity.close_and_drain() {
            self.abort_reset();
            return Err(DeviceError::InvalidState {
                operation: "reset VirtIO PCI transport",
                detail: "queue activity did not drain before the bounded limit".into(),
            });
        }
        #[cfg(test)]
        self.run_reset_before_core_hook();
        if let Err(error) = self.core.reset() {
            self.abort_reset();
            return Err(error);
        }
        self.state.lock().reset(self.core.queue_size_max());
        Ok(self.interrupts.reset())
    }

    /// Publishes the final guest-visible completion of a successful reset.
    ///
    /// The endpoint must call this only after any reset-generated physical
    /// interrupt transition has completed successfully.
    pub fn complete_reset(&self) {
        let mut state = self.state.lock();
        if state.reset_pending {
            self.activity.reopen();
            self.activity.finish_reset();
            state.device_needs_reset = false;
            state.status = 0;
            state.reset_pending = false;
        }
    }

    /// Keeps the reset-required status and closed admission after a reset
    /// step failed.  A later status-zero write may retry the reset, but no
    /// queue processing is admitted until a complete reset is published.
    pub fn abort_reset(&self) {
        let mut state = self.state.lock();
        state.reset_pending = false;
        self.activity.finish_reset();
    }
}
