//! Compatibility constructor for segmented `virtio-net-mmio` devices.

use std::{string::ToString, sync::Arc};

use axdevice::{
    DeviceModel, ResourceRequest, VirtioNetHeaderMode, VirtioNetModel, VirtioNetOptions,
};
use axvmconfig::VirtualDeviceRequest;

use crate::{
    ConfiguredDeviceCatalog, ConfiguredDeviceError, ConfiguredModelRegistration,
    DeviceInstantiationContext,
};

pub(super) const REGISTRATION: ConfiguredModelRegistration = ConfiguredModelRegistration {
    model: "virtio-net-mmio",
    create: create_net,
};

pub(super) fn register(catalog: &mut ConfiguredDeviceCatalog) -> Result<(), ConfiguredDeviceError> {
    catalog.register(module_path!(), REGISTRATION)
}

#[derive(Clone, Copy, Debug, Default, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
enum HeaderMode {
    #[default]
    Negotiated,
    FixedTwelveByte,
}

#[derive(Clone, Copy, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct NetOptions {
    mac_suffix: u8,
    segment_id: u16,
    #[serde(default)]
    header_mode: HeaderMode,
}

fn create_net(
    id: axdevice::DeviceNodeId,
    request: &VirtualDeviceRequest,
    context: &DeviceInstantiationContext,
) -> Result<axdevice::DeviceNodeSpec, ConfiguredDeviceError> {
    let options = request
        .deserialize_options::<NetOptions>()
        .map_err(|error| ConfiguredDeviceError::InvalidOptions {
            device: request.id.clone(),
            model: request.model.clone(),
            detail: error.to_string(),
        })?;
    let controller =
        context
            .default_wired_controller()
            .ok_or_else(|| ConfiguredDeviceError::Instantiation {
                device: request.id.clone(),
                model: request.model.clone(),
                detail: "architecture has no default wired interrupt domain".into(),
            })?;
    let header_mode = match options.header_mode {
        HeaderMode::Negotiated => VirtioNetHeaderMode::Negotiated,
        HeaderMode::FixedTwelveByte => VirtioNetHeaderMode::FixedTwelveByte,
    };
    let model: Arc<dyn DeviceModel> = Arc::new(VirtioNetModel::new(
        request.id.clone(),
        options.mac_suffix,
        VirtioNetOptions {
            segment_id: options.segment_id,
            header_mode,
        },
        controller,
        ResourceRequest::Auto,
        ResourceRequest::Auto,
    ));
    let mut node = axdevice::DeviceNodeSpec::virtual_device(id, model);
    if let Some(controller_node) = context.default_wired_controller_node() {
        node = node.with_dependency(controller_node.clone());
    }
    Ok(node)
}

#[cfg(test)]
mod tests {
    use axdevice_base::InterruptControllerId;

    use super::*;

    fn net_request(header_mode: &str) -> VirtualDeviceRequest {
        VirtualDeviceRequest {
            id: "net0".into(),
            model: "virtio-net-mmio".into(),
            options: toml::Table::from_iter([
                ("mac_suffix".into(), toml::Value::Integer(2)),
                ("segment_id".into(), toml::Value::Integer(7)),
                (
                    "header_mode".into(),
                    toml::Value::String(header_mode.into()),
                ),
            ]),
        }
    }

    fn context() -> DeviceInstantiationContext {
        DeviceInstantiationContext::new().with_default_wired_controller(
            axdevice::DeviceNodeId::new("controller").unwrap(),
            InterruptControllerId::new(0),
        )
    }

    #[test]
    fn legacy_network_model_accepts_fixed_twelve_byte_header_mode() {
        create_net(
            axdevice::DeviceNodeId::new("net0").unwrap(),
            &net_request("fixed-twelve-byte"),
            &context(),
        )
        .unwrap();
    }

    #[test]
    fn legacy_network_model_rejects_unknown_header_mode() {
        assert!(matches!(
            create_net(
                axdevice::DeviceNodeId::new("net0").unwrap(),
                &net_request("legacy"),
                &context(),
            ),
            Err(ConfiguredDeviceError::InvalidOptions { .. })
        ));
    }
}
