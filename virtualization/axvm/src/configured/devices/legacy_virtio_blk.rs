//! Compatibility constructor for file-seeded `virtio-blk-mmio` devices.

use core::fmt::Debug;
use std::{string::ToString, sync::Arc, vec::Vec};

use axdevice::{DeviceModel, MemoryBlockBackend, ResourceRequest, VirtioBlockModel};
use axvmconfig::VirtualDeviceRequest;

use crate::{
    AxVmResult, ConfiguredDeviceCatalog, ConfiguredDeviceError, ConfiguredModelRegistration,
    DeviceInstantiationContext,
};

/// Application-owned source for the initial bytes of a legacy virtual block image.
pub trait VirtioBlockImageProvider: Send + Sync + Debug {
    /// Reads one complete block image from an application-owned path.
    fn read_image(&self, path: &str) -> AxVmResult<Vec<u8>>;
}

/// Provider used when the monitor has not supplied block-image I/O.
#[derive(Debug, Default)]
pub struct NullVirtioBlockImageProvider;

impl VirtioBlockImageProvider for NullVirtioBlockImageProvider {
    fn read_image(&self, path: &str) -> AxVmResult<Vec<u8>> {
        Err(crate::AxVmError::Unsupported {
            operation: "load virtio block image",
            detail: std::format!("no block-image provider is available for `{path}`"),
        })
    }
}

pub(super) const REGISTRATION: ConfiguredModelRegistration = ConfiguredModelRegistration {
    model: "virtio-blk-mmio",
    create: create_block,
};

pub(super) fn register(catalog: &mut ConfiguredDeviceCatalog) -> Result<(), ConfiguredDeviceError> {
    catalog.register(module_path!(), REGISTRATION)
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct BlockOptions {
    image_path: String,
}

fn create_block(
    id: axdevice::DeviceNodeId,
    request: &VirtualDeviceRequest,
    context: &DeviceInstantiationContext,
) -> Result<axdevice::DeviceNodeSpec, ConfiguredDeviceError> {
    let options = request
        .deserialize_options::<BlockOptions>()
        .map_err(|error| ConfiguredDeviceError::InvalidOptions {
            device: request.id.clone(),
            model: request.model.clone(),
            detail: error.to_string(),
        })?;
    if options.image_path.is_empty() {
        return Err(ConfiguredDeviceError::InvalidOptions {
            device: request.id.clone(),
            model: request.model.clone(),
            detail: "image_path must not be empty".into(),
        });
    }
    let controller =
        context
            .default_wired_controller()
            .ok_or_else(|| ConfiguredDeviceError::Instantiation {
                device: request.id.clone(),
                model: request.model.clone(),
                detail: "architecture has no default wired interrupt domain".into(),
            })?;
    let image = context
        .virtio_block_image_provider()
        .read_image(&options.image_path)
        .map_err(|error| ConfiguredDeviceError::Instantiation {
            device: request.id.clone(),
            model: request.model.clone(),
            detail: std::format!("read block image `{}`: {error}", options.image_path),
        })?;
    let backend =
        MemoryBlockBackend::new(image).map_err(|error| ConfiguredDeviceError::Instantiation {
            device: request.id.clone(),
            model: request.model.clone(),
            detail: std::format!("validate block image `{}`: {error}", options.image_path),
        })?;
    let model: Arc<dyn DeviceModel> = Arc::new(VirtioBlockModel::new(
        request.id.clone(),
        Arc::new(backend),
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
    use std::{string::String, sync::Mutex};

    use axdevice_base::InterruptControllerId;

    use super::*;

    #[derive(Debug)]
    struct TestImageProvider {
        requested_path: Mutex<Option<String>>,
    }

    impl VirtioBlockImageProvider for TestImageProvider {
        fn read_image(&self, path: &str) -> AxVmResult<Vec<u8>> {
            *self.requested_path.lock().unwrap() = Some(path.into());
            Ok(std::vec![0; 1024])
        }
    }

    fn block_request(path: &str) -> VirtualDeviceRequest {
        VirtualDeviceRequest {
            id: "disk0".into(),
            model: "virtio-blk-mmio".into(),
            options: toml::Table::from_iter([(
                "image_path".into(),
                toml::Value::String(path.into()),
            )]),
        }
    }

    #[test]
    fn legacy_block_model_reads_the_configured_image_once() {
        let provider = Arc::new(TestImageProvider {
            requested_path: Mutex::new(None),
        });
        let context = DeviceInstantiationContext::new()
            .with_default_wired_controller(
                axdevice::DeviceNodeId::new("controller").unwrap(),
                InterruptControllerId::new(0),
            )
            .with_virtio_block_image_provider(provider.clone());

        create_block(
            axdevice::DeviceNodeId::new("disk0").unwrap(),
            &block_request("/guest/disk.img"),
            &context,
        )
        .unwrap();

        assert_eq!(
            provider.requested_path.lock().unwrap().as_deref(),
            Some("/guest/disk.img")
        );
    }

    #[test]
    fn legacy_block_model_fails_without_an_image_provider() {
        let context = DeviceInstantiationContext::new().with_default_wired_controller(
            axdevice::DeviceNodeId::new("controller").unwrap(),
            InterruptControllerId::new(0),
        );

        assert!(matches!(
            create_block(
                axdevice::DeviceNodeId::new("disk0").unwrap(),
                &block_request("/guest/disk.img"),
                &context,
            ),
            Err(ConfiguredDeviceError::Instantiation { .. })
        ));
    }
}
