//! Runtime and firmware model owned by one device-graph node.

use alloc::{string::String, vec::Vec};

use axdevice_base::InterruptControllerId;

use crate::*;

/// One scalar or marker property understood by firmware composers.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DeviceFirmwareProperty {
    /// A property whose presence alone carries the value.
    Empty { name: String },
    /// One 32-bit integer property.
    U32 { name: String, value: u32 },
    /// One string property.
    String { name: String, value: String },
    /// A property whose value is the resolved controller-local interrupt input.
    InterruptInput { name: String, slot: ResourceSlot },
}

/// A device-tree node whose resource references are resolved by the graph.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FdtNodeSpec {
    node_name: String,
    compatible: Vec<String>,
    register_slots: Vec<ResourceSlot>,
    interrupt_slots: Vec<ResourceSlot>,
    properties: Vec<DeviceFirmwareProperty>,
}

impl FdtNodeSpec {
    /// Creates a node with a stable base name such as `virtio_mmio`.
    pub fn new(node_name: impl Into<String>) -> Self {
        Self {
            node_name: node_name.into(),
            compatible: Vec::new(),
            register_slots: Vec::new(),
            interrupt_slots: Vec::new(),
            properties: Vec::new(),
        }
    }

    /// Adds one compatible string in preference order.
    pub fn with_compatible(mut self, compatible: impl Into<String>) -> Self {
        self.compatible.push(compatible.into());
        self
    }

    /// Adds one graph resource slot to the node's `reg` property.
    pub fn with_register(mut self, slot: ResourceSlot) -> Self {
        self.register_slots.push(slot);
        self
    }

    /// Adds one graph resource slot to the node's interrupt property.
    pub fn with_interrupt(mut self, slot: ResourceSlot) -> Self {
        self.interrupt_slots.push(slot);
        self
    }

    /// Adds one marker property.
    pub fn with_empty_property(mut self, name: impl Into<String>) -> Self {
        self.properties
            .push(DeviceFirmwareProperty::Empty { name: name.into() });
        self
    }

    /// Adds one 32-bit integer property.
    pub fn with_u32_property(mut self, name: impl Into<String>, value: u32) -> Self {
        self.properties.push(DeviceFirmwareProperty::U32 {
            name: name.into(),
            value,
        });
        self
    }

    /// Adds one string property.
    pub fn with_string_property(
        mut self,
        name: impl Into<String>,
        value: impl Into<String>,
    ) -> Self {
        self.properties.push(DeviceFirmwareProperty::String {
            name: name.into(),
            value: value.into(),
        });
        self
    }

    /// Adds a property resolved from one interrupt-resource slot.
    pub fn with_interrupt_input_property(
        mut self,
        name: impl Into<String>,
        slot: ResourceSlot,
    ) -> Self {
        self.properties
            .push(DeviceFirmwareProperty::InterruptInput {
                name: name.into(),
                slot,
            });
        self
    }

    /// Returns the base node name, without a resolved unit address.
    pub fn node_name(&self) -> &str {
        &self.node_name
    }

    /// Returns compatible strings in preference order.
    pub fn compatible(&self) -> &[String] {
        &self.compatible
    }

    /// Returns graph resource slots used by `reg`.
    pub fn register_slots(&self) -> &[ResourceSlot] {
        &self.register_slots
    }

    /// Returns graph resource slots used by the interrupt binding.
    pub fn interrupt_slots(&self) -> &[ResourceSlot] {
        &self.interrupt_slots
    }

    /// Returns additional typed properties.
    pub fn properties(&self) -> &[DeviceFirmwareProperty] {
        &self.properties
    }
}

/// One typed device-tree contribution.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FdtContributionSpec {
    /// An ordinary discoverable device.
    Conventional(FdtNodeSpec),
    /// An interrupt-controller provider.
    InterruptController {
        /// Controller identity used by runtime interrupt endpoints.
        controller: InterruptControllerId,
        /// Firmware node describing this controller.
        node: FdtNodeSpec,
    },
    /// A timer provider.
    Timer(FdtNodeSpec),
    /// A PCI host bridge.
    PciHostBridge(FdtNodeSpec),
    /// A console device.
    Console(FdtNodeSpec),
    /// A firmware transport such as fw_cfg.
    FirmwareTransport(FdtNodeSpec),
}

impl FdtContributionSpec {
    /// Returns the common node declaration carried by this typed contribution.
    pub const fn node(&self) -> &FdtNodeSpec {
        match self {
            Self::Conventional(node)
            | Self::Timer(node)
            | Self::PciHostBridge(node)
            | Self::Console(node)
            | Self::FirmwareTransport(node) => node,
            Self::InterruptController { node, .. } => node,
        }
    }
}

/// An ACPI namespace device whose resources are resolved by the graph.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AcpiDeviceSpec {
    name: String,
    indexed_name: bool,
    hid: Option<String>,
    register_slots: Vec<ResourceSlot>,
    interrupt_slots: Vec<ResourceSlot>,
    properties: Vec<DeviceFirmwareProperty>,
}

impl AcpiDeviceSpec {
    /// Creates one ACPI device declaration.
    pub fn new(name: impl Into<String>, hid: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            indexed_name: false,
            hid: Some(hid.into()),
            register_slots: Vec::new(),
            interrupt_slots: Vec::new(),
            properties: Vec::new(),
        }
    }

    /// Creates an ACPI device whose four-character NameSeg is allocated from
    /// `prefix` for each graph instance.
    ///
    /// Prefixes contain one to three uppercase ASCII letters or digits. The
    /// generic composer appends a zero-padded base-36 instance number.
    pub fn new_indexed(prefix: impl Into<String>, hid: impl Into<String>) -> Self {
        Self {
            name: prefix.into(),
            indexed_name: true,
            hid: Some(hid.into()),
            register_slots: Vec::new(),
            interrupt_slots: Vec::new(),
            properties: Vec::new(),
        }
    }

    /// Creates a table-described contribution with no AML `_HID` device.
    pub fn table(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            indexed_name: false,
            hid: None,
            register_slots: Vec::new(),
            interrupt_slots: Vec::new(),
            properties: Vec::new(),
        }
    }

    /// Adds one graph register-resource slot.
    pub fn with_register(mut self, slot: ResourceSlot) -> Self {
        self.register_slots.push(slot);
        self
    }

    /// Adds one graph interrupt-resource slot.
    pub fn with_interrupt(mut self, slot: ResourceSlot) -> Self {
        self.interrupt_slots.push(slot);
        self
    }

    /// Adds one 32-bit integer property.
    pub fn with_u32_property(mut self, name: impl Into<String>, value: u32) -> Self {
        self.properties.push(DeviceFirmwareProperty::U32 {
            name: name.into(),
            value,
        });
        self
    }

    /// Returns the four-character namespace-local name chosen by the model.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns whether the composer must allocate an instance NameSeg.
    pub const fn has_indexed_name(&self) -> bool {
        self.indexed_name
    }

    /// Returns the ACPI hardware identifier.
    pub fn hid(&self) -> Option<&str> {
        self.hid.as_deref()
    }

    /// Returns graph register-resource slots.
    pub fn register_slots(&self) -> &[ResourceSlot] {
        &self.register_slots
    }

    /// Returns graph interrupt-resource slots.
    pub fn interrupt_slots(&self) -> &[ResourceSlot] {
        &self.interrupt_slots
    }

    /// Returns additional typed properties.
    pub fn properties(&self) -> &[DeviceFirmwareProperty] {
        &self.properties
    }
}

/// One typed ACPI contribution.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AcpiContributionSpec {
    /// An ordinary discoverable device.
    Conventional(AcpiDeviceSpec),
    /// An interrupt-controller provider.
    InterruptController {
        /// Controller identity used by runtime interrupt endpoints.
        controller: InterruptControllerId,
        /// Firmware declaration describing this controller.
        device: AcpiDeviceSpec,
    },
    /// A timer provider.
    Timer(AcpiDeviceSpec),
    /// A PCI host bridge.
    PciHostBridge(AcpiDeviceSpec),
    /// A console device.
    Console(AcpiDeviceSpec),
    /// A firmware transport such as fw_cfg.
    FirmwareTransport(AcpiDeviceSpec),
}

impl AcpiContributionSpec {
    /// Returns the common device declaration carried by this contribution.
    pub const fn device(&self) -> &AcpiDeviceSpec {
        match self {
            Self::Conventional(device)
            | Self::Timer(device)
            | Self::PciHostBridge(device)
            | Self::Console(device)
            | Self::FirmwareTransport(device) => device,
            Self::InterruptController { device, .. } => device,
        }
    }
}

/// Platform interfaces contributed by one virtual-device model.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DeviceFirmwareSpec {
    /// This runtime device intentionally has no guest firmware node.
    None,
    /// Optional FDT and ACPI descriptions for the same device instance.
    Interfaces {
        /// Device-tree contributions, or `None` when FDT is unsupported.
        fdt: Option<Vec<FdtContributionSpec>>,
        /// ACPI contributions, or `None` when ACPI is unsupported.
        acpi: Option<Vec<AcpiContributionSpec>>,
    },
}

impl DeviceFirmwareSpec {
    /// Creates an explicit platform-interface declaration.
    pub const fn interfaces(
        fdt: Option<Vec<FdtContributionSpec>>,
        acpi: Option<Vec<AcpiContributionSpec>>,
    ) -> Self {
        Self::Interfaces { fdt, acpi }
    }

    /// Returns FDT contributions, distinguishing unsupported from supported-empty.
    pub const fn fdt(&self) -> Option<&[FdtContributionSpec]> {
        match self {
            Self::None => None,
            Self::Interfaces { fdt, .. } => match fdt {
                Some(contributions) => Some(contributions.as_slice()),
                None => None,
            },
        }
    }

    /// Returns ACPI contributions, distinguishing unsupported from supported-empty.
    pub const fn acpi(&self) -> Option<&[AcpiContributionSpec]> {
        match self {
            Self::None => None,
            Self::Interfaces { acpi, .. } => match acpi {
                Some(contributions) => Some(contributions.as_slice()),
                None => None,
            },
        }
    }

    pub(crate) fn validate(&self) -> DeviceManagerResult {
        if let Self::Interfaces { fdt, acpi } = self {
            if fdt.is_none() && acpi.is_none() {
                return Err(DeviceManagerError::InvalidConfig {
                    operation: "declare device firmware interfaces",
                    detail: "FDT and ACPI interfaces cannot both be unsupported".into(),
                });
            }
            if fdt.as_ref().is_some_and(Vec::is_empty) || acpi.as_ref().is_some_and(Vec::is_empty) {
                return Err(DeviceManagerError::InvalidConfig {
                    operation: "declare device firmware interfaces",
                    detail: "a supported firmware interface must contribute at least one node"
                        .into(),
                });
            }
        }
        Ok(())
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

    /// Describes every supported guest-firmware interface for this instance.
    fn firmware(&self) -> DeviceFirmwareSpec;

    /// Builds the device while consuming only resources issued by the plan.
    fn build(&self, context: &mut DeviceBuildContext<'_>) -> DeviceManagerResult<DeviceBundle>;
}
