//! Configured constructor for Axvisor IVC channel metadata.

use std::sync::Arc;

use axdevice::*;
use axvmconfig::VirtualDeviceRequest;

use crate::{ConfiguredDeviceError, ConfiguredModelRegistration, DeviceInstantiationContext};

const REGISTERS_SLOT: &str = "registers";

pub(crate) const IVC_REGISTRATIONS: &[ConfiguredModelRegistration] =
    &[ConfiguredModelRegistration {
        model: "ivc-channel",
        create: create_ivc_channel,
    }];

#[derive(Clone, Copy, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct IvcChannelOptions {
    legacy_base_gpa: u64,
    legacy_length: u64,
    notify_irq: Option<usize>,
}

fn create_ivc_channel(
    id: DeviceNodeId,
    request: &VirtualDeviceRequest,
    _context: &DeviceInstantiationContext,
) -> Result<DeviceNodeSpec, ConfiguredDeviceError> {
    let options = request
        .deserialize_options::<IvcChannelOptions>()
        .map_err(|error| ConfiguredDeviceError::InvalidOptions {
            device: request.id.clone(),
            model: request.model.clone(),
            detail: error.to_string(),
        })?;
    Ok(DeviceNodeSpec::virtual_device(
        id,
        Arc::new(IvcChannelModel { options }),
    ))
}

struct IvcChannelModel {
    options: IvcChannelOptions,
}

impl DeviceModel for IvcChannelModel {
    fn requirements(&self) -> DeviceManagerResult<DeviceRequirements> {
        DeviceRequirements::new().with_mmio(
            ResourceSlot::new(REGISTERS_SLOT)?,
            self.options.legacy_length,
            1,
            ResourceRequest::Fixed(self.options.legacy_base_gpa),
        )
    }

    fn firmware(&self) -> DeviceFirmwareSpec {
        let mut spec = DeviceFirmwareSpec::new("ivc-channel")
            .with_compatible("axvisor,ivc-channel")
            .with_register(ResourceSlot::new(REGISTERS_SLOT).expect("static IVC slot is valid"))
            .with_u32_property("axvisor,ivc-version", 1);
        if let Some(notify_irq) = self
            .options
            .notify_irq
            .and_then(|irq| u32::try_from(irq).ok())
        {
            spec = spec.with_u32_property("axvisor,notify-irq", notify_irq);
        }
        spec
    }

    fn build(&self, context: &mut DeviceBuildContext<'_>) -> DeviceManagerResult<DeviceBundle> {
        let (base, length) = context.mmio(REGISTERS_SLOT)?;
        let mut bundle = DeviceBundle::new().with_service::<GuestRangeAllocatorKey>(
            GuestRangePool::new(
                usize::try_from(base).map_err(|_| DeviceManagerError::InvalidConfig {
                    operation: "create IVC channel",
                    detail: "base GPA does not fit usize".into(),
                })?,
                usize::try_from(length).map_err(|_| DeviceManagerError::InvalidConfig {
                    operation: "create IVC channel",
                    detail: "length does not fit usize".into(),
                })?,
            )?
            .into_service(),
        )?;
        if let Some(notify_irq) = self.options.notify_irq {
            bundle.provide_service::<IvcNotifyIrqKey>(Arc::new(notify_irq))?;
        }
        Ok(bundle)
    }
}
