//! Runtime and firmware model owned by one device-graph node.

use alloc::{string::String, vec::Vec};

use crate::*;

/// One scalar property understood by the generic FDT/ACPI composers.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DeviceFirmwareProperty {
    U32 { name: String, value: u32 },
    String { name: String, value: String },
}

/// Firmware metadata for a conventional register-and-interrupt device.
///
/// Architecture-owned devices such as GIC, PLIC and APIC leave this empty and
/// keep using their architecture firmware builders. Ordinary devices can
/// describe their firmware without another trait object or a central device
/// type enum.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DeviceFirmwareSpec {
    node_name: Option<String>,
    compatible: Vec<String>,
    acpi_hid: Option<String>,
    register_slots: Vec<ResourceSlot>,
    interrupt_slots: Vec<ResourceSlot>,
    properties: Vec<DeviceFirmwareProperty>,
}

impl DeviceFirmwareSpec {
    pub fn new(node_name: impl Into<String>) -> Self {
        Self {
            node_name: Some(node_name.into()),
            ..Self::default()
        }
    }

    pub fn with_compatible(mut self, compatible: impl Into<String>) -> Self {
        self.compatible.push(compatible.into());
        self
    }

    pub fn with_acpi_hid(mut self, hid: impl Into<String>) -> Self {
        self.acpi_hid = Some(hid.into());
        self
    }

    pub fn with_register(mut self, slot: ResourceSlot) -> Self {
        self.register_slots.push(slot);
        self
    }

    pub fn with_interrupt(mut self, slot: ResourceSlot) -> Self {
        self.interrupt_slots.push(slot);
        self
    }

    pub fn with_u32_property(mut self, name: impl Into<String>, value: u32) -> Self {
        self.properties.push(DeviceFirmwareProperty::U32 {
            name: name.into(),
            value,
        });
        self
    }

    pub const fn node_name(&self) -> Option<&String> {
        self.node_name.as_ref()
    }

    pub fn compatible(&self) -> &[String] {
        &self.compatible
    }

    pub const fn acpi_hid(&self) -> Option<&String> {
        self.acpi_hid.as_ref()
    }

    pub fn register_slots(&self) -> &[ResourceSlot] {
        &self.register_slots
    }

    pub fn interrupt_slots(&self) -> &[ResourceSlot] {
        &self.interrupt_slots
    }

    pub fn properties(&self) -> &[DeviceFirmwareProperty] {
        &self.properties
    }

    pub fn is_empty(&self) -> bool {
        self.node_name.is_none()
            && self.compatible.is_empty()
            && self.acpi_hid.is_none()
            && self.register_slots.is_empty()
            && self.interrupt_slots.is_empty()
            && self.properties.is_empty()
    }
}

/// Declares and builds one concrete virtual-device instance.
///
/// A model owns its validated, type-specific configuration. The same object is
/// retained from declaration through construction, so the resource plan cannot
/// be paired with a different configuration at build time.
pub trait DeviceModel: Send + Sync {
    /// Declares all named resources required by this instance.
    fn requirements(&self) -> DeviceManagerResult<DeviceRequirements>;

    /// Describes conventional guest-firmware bindings for this instance.
    fn firmware(&self) -> DeviceFirmwareSpec {
        DeviceFirmwareSpec::default()
    }

    /// Builds the device while consuming only resources issued by the plan.
    fn build(&self, context: &mut DeviceBuildContext<'_>) -> DeviceManagerResult<DeviceBundle>;
}
