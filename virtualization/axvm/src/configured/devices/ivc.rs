//! Configured constructor for Axvisor IVC channel metadata.

use std::sync::Arc;

use axdevice::*;
use axdevice_base::{InterruptControllerId, InterruptSharing, InterruptTrigger};
use axvmconfig::VirtualDeviceRequest;

use crate::{
    ConfiguredDeviceError, ConfiguredModelRegistration, DeviceInstantiationContext,
    FixedDeviceBindings, runtime::ivc,
};

const REGISTERS_SLOT: &str = "registers";
const NOTIFY_IRQ_SLOT: &str = "notify";
pub(crate) const IVC_CHANNEL_SHARED_RANGE_SIZE: u64 = ivc::MAX_IVC_CHANNEL_SIZE as u64;

pub(super) fn register(
    catalog: &mut crate::ConfiguredDeviceCatalog,
) -> Result<(), ConfiguredDeviceError> {
    catalog.register(
        module_path!(),
        ConfiguredModelRegistration {
            model: "ivc-channel",
            create: create_ivc_channel,
        },
    )
}

fn request_error(
    request: &VirtualDeviceRequest,
    detail: impl Into<String>,
) -> ConfiguredDeviceError {
    ConfiguredDeviceError::Instantiation {
        device: request.id.clone(),
        model: request.model.clone(),
        detail: detail.into(),
    }
}

fn static_slot(name: &'static str) -> DeviceManagerResult<ResourceSlot> {
    ResourceSlot::new(name)
}

#[derive(Clone, Copy, Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct IvcChannelOptions {}

fn create_ivc_channel(
    id: DeviceNodeId,
    request: &VirtualDeviceRequest,
    context: &DeviceInstantiationContext,
) -> Result<DeviceNodeSpec, ConfiguredDeviceError> {
    request
        .deserialize_options::<IvcChannelOptions>()
        .map_err(|error| ConfiguredDeviceError::InvalidOptions {
            device: request.id.clone(),
            model: request.model.clone(),
            detail: error.to_string(),
        })?;
    let controller = context.default_wired_controller().ok_or_else(|| {
        request_error(
            request,
            "IVC notify requires a default wired interrupt domain",
        )
    })?;
    let dependency = context
        .default_wired_controller_node()
        .ok_or_else(|| {
            request_error(
                request,
                "default wired interrupt domain is missing its device-graph node",
            )
        })?
        .clone();
    Ok(DeviceNodeSpec::virtual_device(
        id,
        Arc::new(IvcChannelModel {
            controller,
            fixed: context.fixed_bindings().clone(),
        }),
    )
    .with_dependency(dependency))
}

struct IvcChannelModel {
    controller: InterruptControllerId,
    fixed: FixedDeviceBindings,
}

impl DeviceModel for IvcChannelModel {
    fn requirements(&self) -> DeviceManagerResult<DeviceRequirements> {
        let register_slot = static_slot(REGISTERS_SLOT)?;
        let irq_slot = static_slot(NOTIFY_IRQ_SLOT)?;
        let mmio_request = self
            .fixed
            .mmio(&register_slot)
            .map_or(ResourceRequest::Auto, |(base, _)| {
                ResourceRequest::Fixed(base)
            });
        let fixed_irq = self.fixed.wired(&irq_slot);
        DeviceRequirements::new()
            .with_mmio(
                register_slot,
                IVC_CHANNEL_SHARED_RANGE_SIZE,
                0x1000,
                mmio_request,
            )?
            .with_wired_irq(
                irq_slot,
                fixed_irq.map_or(self.controller, |binding| binding.controller),
                fixed_irq.map_or(InterruptTrigger::EdgeTriggered, |binding| binding.trigger),
                fixed_irq.map_or(InterruptSharing::Exclusive, |binding| binding.sharing),
                fixed_irq.map_or(ResourceRequest::Auto, |binding| {
                    ResourceRequest::Fixed(binding.input)
                }),
            )
    }

    fn firmware(&self) -> DeviceFirmwareSpec {
        DeviceFirmwareSpec::interfaces(
            Some(std::vec![FdtContributionSpec::Conventional(
                FdtNodeSpec::new("ivc-channel")
                    .with_compatible("axvisor,ivc-channel")
                    .with_register(
                        ResourceSlot::new(REGISTERS_SLOT).expect("static IVC slot is valid"),
                    )
                    .with_interrupt(
                        ResourceSlot::new(NOTIFY_IRQ_SLOT).expect("static IVC slot is valid"),
                    )
                    .with_interrupt_input_property(
                        "axvisor,notify-irq",
                        ResourceSlot::new(NOTIFY_IRQ_SLOT).expect("static IVC slot is valid"),
                    )
                    .with_string_property("status", "okay")
                    .with_u32_property("axvisor,ivc-version", 1),
            )]),
            None,
        )
    }

    fn build(&self, context: &mut DeviceBuildContext<'_>) -> DeviceManagerResult<DeviceBundle> {
        let (base, length) = context.mmio(REGISTERS_SLOT)?;
        let notify_irq = context.irq(NOTIFY_IRQ_SLOT)?;
        let bundle = DeviceBundle::new()
            .with_service::<ivc::IvcApertureAllocatorKey>(
                ivc::IvcAperturePool::new(
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
            )?
            .with_service::<ivc::IvcNotifyEndpointKey>(Arc::new(
                ivc::WiredIvcNotifyEndpoint::new(notify_irq),
            ))?;
        Ok(bundle)
    }
}
