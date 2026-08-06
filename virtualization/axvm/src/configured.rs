//! Code-registered configuration factories for open-ended virtual-device models.

use alloc::{collections::BTreeMap, string::String, sync::Arc, vec::Vec};
use core::fmt;

use axdevice::*;
use axdevice_base::InterruptControllerId;
use axvmconfig::VirtualDeviceRequest;

use crate::{architecture::*, machine::*, *};

pub trait ConfiguredDeviceFactory: Send + Sync {
    fn model_name(&self) -> &'static str;

    fn instantiate(
        &self,
        request: &VirtualDeviceRequest,
        context: &DeviceInstantiationContext,
    ) -> Result<ConfiguredDeviceInstance, ConfiguredDeviceError>;
}

#[derive(Clone, Debug)]
pub struct DeviceInstantiationContext {
    architecture: MachineArchitecture,
    default_wired_controller: Option<(DeviceNodeId, InterruptControllerId)>,
}

impl DeviceInstantiationContext {
    pub const fn new(architecture: MachineArchitecture) -> Self {
        Self {
            architecture,
            default_wired_controller: None,
        }
    }

    pub const fn architecture(&self) -> MachineArchitecture {
        self.architecture
    }

    pub fn with_default_wired_controller(
        mut self,
        node: DeviceNodeId,
        controller: InterruptControllerId,
    ) -> Self {
        self.default_wired_controller = Some((node, controller));
        self
    }

    pub fn default_wired_controller(&self) -> Option<InterruptControllerId> {
        self.default_wired_controller
            .as_ref()
            .map(|(_, controller)| *controller)
    }

    /// Returns the graph node that must precede users of the default wired domain.
    pub fn default_wired_controller_node(&self) -> Option<&DeviceNodeId> {
        self.default_wired_controller.as_ref().map(|(node, _)| node)
    }
}

pub struct ConfiguredDeviceInstance {
    model: Arc<dyn DeviceModel>,
    firmware: FirmwareModels,
    dependencies: Vec<DeviceNodeId>,
}

impl ConfiguredDeviceInstance {
    pub fn new(model: Arc<dyn DeviceModel>) -> Self {
        Self {
            model,
            firmware: FirmwareModels::default(),
            dependencies: Vec::new(),
        }
    }

    pub fn with_firmware(mut self, firmware: FirmwareModels) -> Self {
        self.firmware = firmware;
        self
    }

    pub fn with_dependency(mut self, dependency: DeviceNodeId) -> Self {
        self.dependencies.push(dependency);
        self
    }

    fn into_node(self, id: DeviceNodeId) -> DeviceNodeSpec {
        let mut node =
            DeviceNodeSpec::virtual_device(id, self.model).with_firmware_models(self.firmware);
        for dependency in self.dependencies {
            node = node.with_dependency(dependency);
        }
        node
    }
}

#[derive(Default)]
pub struct ConfiguredDeviceCatalog {
    factories: BTreeMap<String, Arc<dyn ConfiguredDeviceFactory>>,
}

impl ConfiguredDeviceCatalog {
    pub const fn new() -> Self {
        Self {
            factories: BTreeMap::new(),
        }
    }

    pub fn register(
        &mut self,
        factory: Arc<dyn ConfiguredDeviceFactory>,
    ) -> Result<(), ConfiguredDeviceError> {
        let name = factory.model_name();
        validate_model_name(name)?;
        if self.factories.contains_key(name) {
            return Err(ConfiguredDeviceError::DuplicateModel { model: name.into() });
        }
        self.factories.insert(name.into(), factory);
        Ok(())
    }

    fn instantiate(
        &self,
        request: &VirtualDeviceRequest,
        context: &DeviceInstantiationContext,
    ) -> Result<ConfiguredDeviceInstance, ConfiguredDeviceError> {
        self.factories
            .get(&request.model)
            .ok_or_else(|| ConfiguredDeviceError::UnknownVirtualDeviceModel {
                model: request.model.clone(),
            })?
            .instantiate(request, context)
    }

    pub fn instantiate_node(
        &self,
        request: &VirtualDeviceRequest,
        context: &DeviceInstantiationContext,
    ) -> Result<DeviceNodeSpec, ConfiguredDeviceError> {
        let instance = self.instantiate(request, context)?;
        let id = DeviceNodeId::new(request.id.clone()).map_err(|error| {
            ConfiguredDeviceError::InvalidDeviceId {
                device: request.id.clone(),
                detail: alloc::format!("{error}"),
            }
        })?;
        Ok(instance.into_node(id))
    }
}

impl fmt::Debug for ConfiguredDeviceCatalog {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ConfiguredDeviceCatalog")
            .field("models", &self.factories.keys().collect::<Vec<_>>())
            .finish()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ConfiguredDeviceError {
    #[error("unknown virtual device model '{model}'")]
    UnknownVirtualDeviceModel { model: String },
    #[error("virtual device model '{model}' is registered more than once")]
    DuplicateModel { model: String },
    #[error("invalid virtual device model name '{model}'")]
    InvalidModelName { model: String },
    #[error("invalid options for virtual device '{device}' ({model}): {detail}")]
    InvalidOptions {
        device: String,
        model: String,
        detail: String,
    },
    #[error("failed to instantiate virtual device '{device}' ({model}): {detail}")]
    Instantiation {
        device: String,
        model: String,
        detail: String,
    },
    #[error("invalid virtual device id '{device}': {detail}")]
    InvalidDeviceId { device: String, detail: String },
}

pub(crate) fn append_configured_devices(
    config: &crate::config::AxVMConfig,
    nodes: &mut Vec<DeviceNodeSpec>,
    default_controller: &DeviceNodeId,
) -> AxVmResult {
    let context = DeviceInstantiationContext::new(crate::arch::CurrentArch::MACHINE_ARCHITECTURE)
        .with_default_wired_controller(default_controller.clone(), InterruptControllerId::new(0));
    for request in config.virtual_device_requests() {
        let node = config
            .virtual_device_catalog()
            .instantiate_node(request, &context)
            .map_err(|error| AxVmError::invalid_config(alloc::format!("{error}")))?;
        nodes.push(node);
    }
    Ok(())
}

fn validate_model_name(name: &str) -> Result<(), ConfiguredDeviceError> {
    let valid = !name.is_empty()
        && name.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'.')
        });
    if valid {
        Ok(())
    } else {
        Err(ConfiguredDeviceError::InvalidModelName { model: name.into() })
    }
}
