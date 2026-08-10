//! Code-registered constructors for open-ended virtual-device models.

use core::fmt;
use std::{collections::BTreeMap, string::String, sync::Arc, vec::Vec};

use axdevice::*;
use axdevice_base::{ControllerInputId, InterruptControllerId, InterruptSharing, InterruptTrigger};
use axvmconfig::VirtualDeviceRequest;

use crate::{machine::GuestSerialFirmwareIdentity, *};

mod append;

pub use append::DefaultVirtualDeviceIntent;
pub(crate) use append::append_configured_devices;

/// Creates one graph node from a validated, model-specific request.
pub type ConfiguredModelConstructor = for<'a> fn(
    DeviceNodeId,
    &VirtualDeviceRequest,
    &'a DeviceInstantiationContext,
) -> Result<DeviceNodeSpec, ConfiguredDeviceError>;

/// One explicit catalog entry. Adding a device changes its module and the
/// catalog assembly site, not a framework-wide device enum.
#[derive(Clone, Copy)]
pub struct ConfiguredModelRegistration {
    pub model: &'static str,
    pub create: ConfiguredModelConstructor,
}

#[derive(Clone, Debug)]
pub struct FixedWiredBinding {
    pub controller: InterruptControllerId,
    pub input: ControllerInputId,
    pub trigger: InterruptTrigger,
    pub sharing: InterruptSharing,
}

/// Planner-only fixed resources derived from a machine profile or host
/// firmware. These values never cross the user configuration boundary.
#[derive(Clone, Debug, Default)]
pub struct FixedDeviceBindings {
    mmio: BTreeMap<ResourceSlot, (u64, u64)>,
    pio: BTreeMap<ResourceSlot, (u16, u16)>,
    wired: BTreeMap<ResourceSlot, FixedWiredBinding>,
}

impl FixedDeviceBindings {
    pub fn with_mmio(mut self, slot: ResourceSlot, base: u64, size: u64) -> Self {
        self.mmio.insert(slot, (base, size));
        self
    }

    pub fn with_pio(mut self, slot: ResourceSlot, base: u16, size: u16) -> Self {
        self.pio.insert(slot, (base, size));
        self
    }

    pub fn with_wired(mut self, slot: ResourceSlot, binding: FixedWiredBinding) -> Self {
        self.wired.insert(slot, binding);
        self
    }

    pub fn mmio(&self, slot: &ResourceSlot) -> Option<(u64, u64)> {
        self.mmio.get(slot).copied()
    }

    pub fn pio(&self, slot: &ResourceSlot) -> Option<(u16, u16)> {
        self.pio.get(slot).copied()
    }

    pub fn wired(&self, slot: &ResourceSlot) -> Option<&FixedWiredBinding> {
        self.wired.get(slot)
    }
}

#[derive(Clone)]
pub struct DeviceInstantiationContext {
    default_wired_controller: Option<(DeviceNodeId, InterruptControllerId)>,
    fixed: FixedDeviceBindings,
    firmware_binding: DeviceFirmwareBinding,
    serial_profile: Option<crate::machine::GuestSerialProfile>,
    serial_backend_factory: Arc<dyn SerialBackendFactory>,
    host_console_by_default: bool,
}

impl DeviceInstantiationContext {
    pub fn new() -> Self {
        Self {
            default_wired_controller: None,
            fixed: FixedDeviceBindings::default(),
            firmware_binding: DeviceFirmwareBinding::None,
            serial_profile: None,
            serial_backend_factory: Arc::new(NullSerialBackendFactory),
            host_console_by_default: false,
        }
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

    pub fn fixed_bindings(&self) -> &FixedDeviceBindings {
        &self.fixed
    }

    pub fn firmware_binding(&self) -> &DeviceFirmwareBinding {
        &self.firmware_binding
    }

    pub(crate) fn with_serial_defaults(
        mut self,
        profile: crate::machine::GuestSerialProfile,
        backend_factory: Arc<dyn SerialBackendFactory>,
        fixed: FixedDeviceBindings,
        firmware_binding: DeviceFirmwareBinding,
        host_console_by_default: bool,
    ) -> Self {
        self.serial_profile = Some(profile);
        self.serial_backend_factory = backend_factory;
        self.fixed = fixed;
        self.firmware_binding = firmware_binding;
        self.host_console_by_default = host_console_by_default;
        self
    }

    pub(crate) const fn serial_profile(&self) -> Option<crate::machine::GuestSerialProfile> {
        self.serial_profile
    }

    pub(crate) fn serial_backend_factory(&self) -> Arc<dyn SerialBackendFactory> {
        self.serial_backend_factory.clone()
    }

    pub(crate) const fn host_console_by_default(&self) -> bool {
        self.host_console_by_default
    }
}

impl Default for DeviceInstantiationContext {
    fn default() -> Self {
        Self::new()
    }
}

pub struct ConfiguredDeviceCatalog {
    registrations: BTreeMap<String, ConfiguredModelRegistration>,
}

impl ConfiguredDeviceCatalog {
    pub fn new() -> Self {
        let mut catalog = Self {
            registrations: BTreeMap::new(),
        };
        for registration in crate::machine::SERIAL_REGISTRATIONS {
            let previous = catalog
                .registrations
                .insert(registration.model.into(), *registration);
            debug_assert!(previous.is_none());
        }
        for registration in VPCI_REGISTRATIONS {
            let previous = catalog
                .registrations
                .insert(registration.model.into(), *registration);
            debug_assert!(previous.is_none());
        }
        catalog
    }

    pub fn register(
        &mut self,
        registration: ConfiguredModelRegistration,
    ) -> Result<(), ConfiguredDeviceError> {
        let name = registration.model;
        validate_model_name(name)?;
        if self.registrations.contains_key(name) {
            return Err(ConfiguredDeviceError::DuplicateModel { model: name.into() });
        }
        self.registrations.insert(name.into(), registration);
        Ok(())
    }

    pub fn instantiate_node(
        &self,
        request: &VirtualDeviceRequest,
        context: &DeviceInstantiationContext,
    ) -> Result<DeviceNodeSpec, ConfiguredDeviceError> {
        let id = DeviceNodeId::new(request.id.clone()).map_err(|error| {
            ConfiguredDeviceError::InvalidDeviceId {
                device: request.id.clone(),
                detail: std::format!("{error}"),
            }
        })?;
        let registration = self.registrations.get(&request.model).ok_or_else(|| {
            ConfiguredDeviceError::UnknownVirtualDeviceModel {
                model: request.model.clone(),
            }
        })?;
        (registration.create)(id, request, context)
    }
}

impl Default for ConfiguredDeviceCatalog {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for ConfiguredDeviceCatalog {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ConfiguredDeviceCatalog")
            .field("models", &self.registrations.keys().collect::<Vec<_>>())
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

const VPCI_REGISTRATIONS: &[ConfiguredModelRegistration] = &[
    ConfiguredModelRegistration {
        model: "virtual-pci-host",
        create: create_virtual_pci_host,
    },
    ConfiguredModelRegistration {
        model: "ivshmem-pci",
        create: create_ivshmem_pci,
    },
];

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct VirtualPciOptions {
    ecam_base: usize,
    ecam_size: usize,
    #[serde(default)]
    legacy_irq: usize,
    #[serde(default)]
    cfg_list: Vec<usize>,
}

fn create_virtual_pci_host(
    id: DeviceNodeId,
    request: &VirtualDeviceRequest,
    _context: &DeviceInstantiationContext,
) -> Result<DeviceNodeSpec, ConfiguredDeviceError> {
    let options = vpci_options(request)?;
    let model = VirtualPciHostModel::from_cfg_list(
        request.id.clone(),
        GuestPhysAddr::from(options.ecam_base),
        options.ecam_size,
        &options.cfg_list,
    )
    .map_err(|error| ConfiguredDeviceError::Instantiation {
        device: request.id.clone(),
        model: request.model.clone(),
        detail: error.to_string(),
    })?;
    Ok(DeviceNodeSpec::virtual_device(id, Arc::new(model)))
}

fn create_ivshmem_pci(
    id: DeviceNodeId,
    request: &VirtualDeviceRequest,
    context: &DeviceInstantiationContext,
) -> Result<DeviceNodeSpec, ConfiguredDeviceError> {
    let options = vpci_options(request)?;
    let irq = if options.legacy_irq == 0 {
        None
    } else {
        let controller = context.default_wired_controller().ok_or_else(|| {
            ConfiguredDeviceError::Instantiation {
                device: request.id.clone(),
                model: request.model.clone(),
                detail: "architecture has no default wired interrupt domain".into(),
            }
        })?;
        Some((controller, ControllerInputId::new(options.legacy_irq)))
    };
    let model = VirtualPciHostModel::ivshmem_from_cfg_list(
        request.id.clone(),
        GuestPhysAddr::from(options.ecam_base),
        options.ecam_size,
        irq,
        &options.cfg_list,
    )
    .map_err(|error| ConfiguredDeviceError::Instantiation {
        device: request.id.clone(),
        model: request.model.clone(),
        detail: error.to_string(),
    })?;
    let mut node = DeviceNodeSpec::virtual_device(id, Arc::new(model));
    if irq.is_some()
        && let Some(controller_node) = context.default_wired_controller_node()
    {
        node = node.with_dependency(controller_node.clone());
    }
    Ok(node)
}

fn vpci_options(
    request: &VirtualDeviceRequest,
) -> Result<VirtualPciOptions, ConfiguredDeviceError> {
    request
        .deserialize_options::<VirtualPciOptions>()
        .map_err(|error| ConfiguredDeviceError::InvalidOptions {
            device: request.id.clone(),
            model: request.model.clone(),
            detail: error.to_string(),
        })
}
