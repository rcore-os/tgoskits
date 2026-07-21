//! Move-only registration for call-stack-only software block devices.

use alloc::vec::Vec;

use ax_errno::AxError;
use rdif_block::{
    CompletedRequest, DeviceInfo, HardwareQueueLimits, InlineBlockDevice, OwnedRequest,
};
use rdrive::{Device, DeviceId, DriverGeneric};

use super::BlockDeviceBinding;
use crate::{
    BindingInfo,
    registration::{BoundDevice, register_bound_device},
};

/// Registry owner retained until the block runtime takes the inline queue.
struct PlatformInlineBlockDevice {
    device: Option<InlineBlockDevice>,
    binding: BlockDeviceBinding,
}

impl DriverGeneric for PlatformInlineBlockDevice {
    fn name(&self) -> &str {
        self.device
            .as_ref()
            .map_or("taken-inline-block-device", InlineBlockDevice::name)
    }
}

impl BoundDevice for PlatformInlineBlockDevice {
    fn binding_info(&self) -> &BindingInfo {
        self.binding.platform_binding()
    }
}

/// Software block device transferred from discovery to the runtime.
#[must_use = "publish or retain the move-only inline block device"]
pub struct RegisteredInlineBlockDevice {
    device: InlineBlockDevice,
    binding: BlockDeviceBinding,
}

impl RegisteredInlineBlockDevice {
    /// Returns the registry name without exposing its queue owner.
    pub fn name(&self) -> &str {
        self.device.name()
    }

    /// Returns immutable logical block geometry.
    pub const fn device_info(&self) -> DeviceInfo {
        self.device.device_info()
    }

    /// Returns request limits with no runtime watchdog policy.
    pub const fn limits(&self) -> HardwareQueueLimits {
        self.device.limits()
    }

    /// Returns the stable platform registry identity.
    pub const fn device_id(&self) -> DeviceId {
        self.binding.device_id()
    }

    /// Executes one request without allocating a tag or scheduling work.
    pub fn execute_owned(&mut self, request: OwnedRequest) -> CompletedRequest {
        self.device.execute_owned(request)
    }

    /// Transfers the complete software device and its discovery binding.
    pub fn into_parts(self) -> (InlineBlockDevice, BlockDeviceBinding) {
        (self.device, self.binding)
    }
}

impl TryFrom<Device<PlatformInlineBlockDevice>> for RegisteredInlineBlockDevice {
    type Error = AxError;

    fn try_from(base: Device<PlatformInlineBlockDevice>) -> Result<Self, Self::Error> {
        let mut registered = base.lock().map_err(|_| AxError::BadState)?;
        let device = registered.device.take().ok_or(AxError::BadState)?;
        Ok(Self {
            device,
            binding: registered.binding.clone(),
        })
    }
}

/// Registers software devices that always return ownership in the call stack.
pub trait PlatformInlineBlock {
    /// Registers an inline device with no platform resource metadata.
    fn register_inline_block(self, device: InlineBlockDevice) -> Option<usize>;

    /// Registers an inline device with immutable discovery metadata.
    ///
    /// Inline devices may describe memory resources, but an IRQ binding is an
    /// invalid configuration because this boundary has no asynchronous owner.
    fn register_inline_block_with_info(
        self,
        device: InlineBlockDevice,
        binding: BindingInfo,
    ) -> Option<usize>;
}

impl PlatformInlineBlock for rdrive::PlatformDevice {
    fn register_inline_block(self, device: InlineBlockDevice) -> Option<usize> {
        self.register_inline_block_with_info(device, BindingInfo::empty())
    }

    fn register_inline_block_with_info(
        self,
        device: InlineBlockDevice,
        binding: BindingInfo,
    ) -> Option<usize> {
        if !binding.irq_sources().is_empty() {
            log::warn!(
                "refusing to attach IRQ resources to inline block device {}",
                device.name()
            );
            return None;
        }
        let device_id = self.descriptor().device_id();
        register_bound_device(
            self,
            PlatformInlineBlockDevice {
                device: Some(device),
                binding: BlockDeviceBinding::new(device_id, binding),
            },
        )
    }
}

/// Transfers every registered inline device to the block runtime exactly once.
pub fn take_inline_block_devices() -> Vec<RegisteredInlineBlockDevice> {
    rdrive::get_list::<PlatformInlineBlockDevice>()
        .into_iter()
        .filter_map(
            |device| match RegisteredInlineBlockDevice::try_from(device) {
                Ok(device) => Some(device),
                Err(error) => {
                    log::warn!("failed to take inline block device: {error:?}");
                    None
                }
            },
        )
        .collect()
}
