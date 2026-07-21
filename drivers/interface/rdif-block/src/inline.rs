//! Call-stack-only block devices with no asynchronous runtime ownership.

use alloc::{boxed::Box, string::String};

use crate::{CompletedRequest, DeviceInfo, HardwareQueueLimits, InlineExecuteQueue, OwnedRequest};

/// Move-only software block device whose request ownership never leaves a call.
///
/// This object deliberately has no controller, IRQ, tag, watchdog, or
/// maintenance-domain surface. An OS may protect it with an ordinary
/// task-context lock when several callers share one software device.
#[must_use = "register or retain the inline block device"]
pub struct InlineBlockDevice {
    name: String,
    device: DeviceInfo,
    limits: HardwareQueueLimits,
    queue: Box<dyn InlineExecuteQueue>,
}

impl InlineBlockDevice {
    /// Joins immutable geometry with its one move-only inline queue.
    ///
    /// # Errors
    ///
    /// Returns [`InlineBlockDeviceError`] when the published geometry or
    /// request limits cannot describe a usable block device.
    pub fn new<Q>(
        name: impl Into<String>,
        device: DeviceInfo,
        limits: HardwareQueueLimits,
        queue: Q,
    ) -> Result<Self, InlineBlockDeviceError>
    where
        Q: InlineExecuteQueue,
    {
        let name = name.into();
        validate_inline_metadata(&name, device, limits)?;
        Ok(Self {
            name,
            device,
            limits,
            queue: Box::new(queue),
        })
    }

    /// Returns the stable device name used by the OS registry.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns immutable logical block geometry.
    pub const fn device_info(&self) -> DeviceInfo {
        self.device
    }

    /// Returns hardware-independent request limits without an OS watchdog.
    pub const fn limits(&self) -> HardwareQueueLimits {
        self.limits
    }

    /// Executes one request and returns its complete ownership in this call.
    pub fn execute_owned(&mut self, request: OwnedRequest) -> CompletedRequest {
        self.queue.execute_owned(request)
    }
}

/// Invalid metadata at the inline-only publication boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum InlineBlockDeviceError {
    /// Registry names identify devices in diagnostics and must not be empty.
    #[error("inline block device name is empty")]
    EmptyName,
    /// Capacity or logical block size cannot describe a usable address space.
    #[error("inline block device geometry is invalid")]
    InvalidGeometry,
    /// Request limits cannot validate any legal data transfer.
    #[error("inline block device request limits are invalid")]
    InvalidLimits,
}

fn validate_inline_metadata(
    name: &str,
    device: DeviceInfo,
    limits: HardwareQueueLimits,
) -> Result<(), InlineBlockDeviceError> {
    if name.is_empty() {
        return Err(InlineBlockDeviceError::EmptyName);
    }
    if device.num_blocks == 0
        || device.logical_block_size == 0
        || !device.logical_block_size.is_power_of_two()
    {
        return Err(InlineBlockDeviceError::InvalidGeometry);
    }
    if limits.dma_alignment == 0
        || !limits.dma_alignment.is_power_of_two()
        || limits.max_blocks_per_request == 0
        || limits.max_segments == 0
        || limits.max_segment_size < device.logical_block_size
    {
        return Err(InlineBlockDeviceError::InvalidLimits);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RequestId;

    struct EchoQueue;

    impl InlineExecuteQueue for EchoQueue {
        fn execute_owned(&mut self, request: OwnedRequest) -> CompletedRequest {
            CompletedRequest::new(RequestId::INLINE, Ok(()), request)
        }
    }

    #[test]
    fn invalid_metadata_is_rejected_before_registry_publication() {
        let error = InlineBlockDevice::new(
            "inline",
            DeviceInfo::new(0, 512),
            HardwareQueueLimits::simple(512, u64::MAX),
            EchoQueue,
        )
        .err()
        .expect("zero-capacity device must be rejected");

        assert_eq!(error, InlineBlockDeviceError::InvalidGeometry);
    }
}
