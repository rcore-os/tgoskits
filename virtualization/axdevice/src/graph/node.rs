//! Plain device-node declarations.

use alloc::{string::String, sync::Arc, vec::Vec};
use core::fmt;

use crate::*;

/// Stable identity of one node in a VM-local device graph.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct DeviceNodeId(String);

impl DeviceNodeId {
    /// Creates a validated stable node identity.
    pub fn new(value: impl Into<String>) -> DeviceManagerResult<Self> {
        let value = value.into();
        let valid = !value.is_empty()
            && value.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'@')
            });
        if !valid {
            return Err(DeviceManagerError::InvalidInput {
                operation: "create device graph node identifier",
                detail: alloc::format!("'{value}' is not a stable device identifier"),
            });
        }
        Ok(Self(value))
    }

    /// Returns the stable textual identity.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DeviceNodeId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Ownership and firmware semantics of a graph node.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeviceNodeKind {
    /// A device built entirely by the VMM.
    Virtual,
    /// A host device whose firmware identity and resources are preserved.
    HostPassthrough,
    /// A virtual implementation replacing a host firmware device.
    HostReplacement,
    /// A node emitted only into guest firmware.
    FirmwareOnly,
}

impl DeviceNodeKind {
    pub(crate) const fn requires_factory(self) -> bool {
        matches!(self, Self::Virtual | Self::HostReplacement)
    }
}

/// Firmware identity retained independently from runtime device state.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum DeviceFirmwareBinding {
    /// A source or destination FDT node path.
    FdtNode(String),
    /// A normalized ACPI namespace path.
    AcpiDevice(String),
    /// No direct firmware node is emitted for this runtime node.
    #[default]
    None,
}

/// One normalized host range retained by a passthrough graph node.
///
/// The graph owns this plain descriptor; it never retains a firmware parser or
/// a borrowed host-device object. The guest range is also declared as a fixed
/// MMIO resource so passthrough and emulated devices participate in the same
/// conflict transaction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HostPassthroughMapping {
    guest_base: u64,
    host_base: u64,
    length: u64,
}

impl HostPassthroughMapping {
    /// Creates a checked linear host mapping.
    pub fn new(guest_base: u64, host_base: u64, length: u64) -> DeviceManagerResult<Self> {
        if length == 0
            || guest_base.checked_add(length).is_none()
            || host_base.checked_add(length).is_none()
        {
            return Err(DeviceManagerError::InvalidConfig {
                operation: "snapshot host passthrough mapping",
                detail: alloc::format!(
                    "invalid mapping [{guest_base:#x}, +{length:#x}) -> {host_base:#x}"
                ),
            });
        }
        Ok(Self {
            guest_base,
            host_base,
            length,
        })
    }

    /// Returns the guest-visible base address.
    pub const fn guest_base(self) -> u64 {
        self.guest_base
    }

    /// Returns the host physical base address.
    pub const fn host_base(self) -> u64 {
        self.host_base
    }

    /// Returns the mapped byte length.
    pub const fn length(self) -> u64 {
        self.length
    }
}

/// One unsealed device graph node.
pub struct DeviceNodeSpec {
    pub(crate) id: DeviceNodeId,
    pub(crate) kind: DeviceNodeKind,
    pub(crate) parent: Option<DeviceNodeId>,
    pub(crate) dependencies: Vec<DeviceNodeId>,
    pub(crate) firmware: DeviceFirmwareBinding,
    pub(crate) firmware_spec: DeviceFirmwareSpec,
    pub(crate) model: Option<Arc<dyn DeviceModel>>,
    pub(crate) requirements: Option<DeviceRequirements>,
    pub(crate) host_mapping: Option<HostPassthroughMapping>,
}

impl DeviceNodeSpec {
    /// Returns this declaration's stable graph identity.
    pub const fn id(&self) -> &DeviceNodeId {
        &self.id
    }

    /// Creates a runtime-backed virtual node.
    pub fn virtual_device(id: DeviceNodeId, model: Arc<dyn DeviceModel>) -> Self {
        Self::runtime(id, DeviceNodeKind::Virtual, model)
    }

    /// Creates a virtual replacement for one host-described device.
    pub fn host_replacement(id: DeviceNodeId, model: Arc<dyn DeviceModel>) -> Self {
        Self::runtime(id, DeviceNodeKind::HostReplacement, model)
    }

    /// Creates a host passthrough node with fixed resource reservations.
    pub fn host_passthrough(id: DeviceNodeId, requirements: DeviceRequirements) -> Self {
        Self {
            id,
            kind: DeviceNodeKind::HostPassthrough,
            parent: None,
            dependencies: Vec::new(),
            firmware: DeviceFirmwareBinding::None,
            firmware_spec: DeviceFirmwareSpec::None,
            model: None,
            requirements: Some(requirements),
            host_mapping: None,
        }
    }

    /// Creates a firmware-only container or provider node.
    pub fn firmware_only(id: DeviceNodeId) -> Self {
        Self {
            id,
            kind: DeviceNodeKind::FirmwareOnly,
            parent: None,
            dependencies: Vec::new(),
            firmware: DeviceFirmwareBinding::None,
            firmware_spec: DeviceFirmwareSpec::None,
            model: None,
            requirements: Some(DeviceRequirements::new()),
            host_mapping: None,
        }
    }

    fn runtime(id: DeviceNodeId, kind: DeviceNodeKind, model: Arc<dyn DeviceModel>) -> Self {
        let firmware_spec = model.firmware();
        Self {
            id,
            kind,
            parent: None,
            dependencies: Vec::new(),
            firmware: DeviceFirmwareBinding::None,
            firmware_spec,
            model: Some(model),
            requirements: None,
            host_mapping: None,
        }
    }

    /// Places this node below a firmware parent.
    pub fn with_parent(mut self, parent: DeviceNodeId) -> Self {
        self.parent = Some(parent);
        self
    }

    /// Adds an explicit construction dependency.
    pub fn with_dependency(mut self, dependency: DeviceNodeId) -> Self {
        self.dependencies.push(dependency);
        self
    }

    /// Associates this node with normalized firmware identity.
    pub fn with_firmware_binding(mut self, binding: DeviceFirmwareBinding) -> Self {
        self.firmware = binding;
        self
    }

    /// Retains the checked host mapping represented by this passthrough node.
    pub fn with_host_mapping(mut self, mapping: HostPassthroughMapping) -> Self {
        self.host_mapping = Some(mapping);
        self
    }
}

#[cfg(test)]
mod tests {
    use alloc::sync::Arc;
    use core::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    struct CountingFirmwareModel {
        calls: Arc<AtomicUsize>,
    }

    struct StaticFirmwareModel(DeviceFirmwareSpec);

    impl DeviceModel for CountingFirmwareModel {
        fn requirements(&self) -> DeviceManagerResult<DeviceRequirements> {
            Ok(DeviceRequirements::new())
        }

        fn firmware(&self) -> DeviceFirmwareSpec {
            self.calls.fetch_add(1, Ordering::Relaxed);
            DeviceFirmwareSpec::interfaces(
                Some(alloc::vec![FdtContributionSpec::Conventional(
                    FdtNodeSpec::new("counted"),
                )]),
                None,
            )
        }

        fn build(
            &self,
            _context: &mut DeviceBuildContext<'_>,
        ) -> DeviceManagerResult<DeviceBundle> {
            unreachable!("the declaration regression does not build the device")
        }
    }

    impl DeviceModel for StaticFirmwareModel {
        fn requirements(&self) -> DeviceManagerResult<DeviceRequirements> {
            Ok(DeviceRequirements::new())
        }

        fn firmware(&self) -> DeviceFirmwareSpec {
            self.0.clone()
        }

        fn build(
            &self,
            _context: &mut DeviceBuildContext<'_>,
        ) -> DeviceManagerResult<DeviceBundle> {
            unreachable!("firmware support tests do not build devices")
        }
    }

    fn resolved_firmware(spec: DeviceFirmwareSpec) -> ResolvedDeviceGraph {
        let mut graph = DeviceGraphBuilder::new();
        graph
            .add(DeviceNodeSpec::virtual_device(
                DeviceNodeId::new("firmware-matrix").unwrap(),
                Arc::new(StaticFirmwareModel(spec)),
            ))
            .unwrap();
        graph
            .declare()
            .unwrap()
            .resolve(ResourcePools::new())
            .unwrap()
    }

    #[test]
    fn runtime_node_freezes_firmware_when_declared() {
        let calls = Arc::new(AtomicUsize::new(0));
        let model = Arc::new(CountingFirmwareModel {
            calls: Arc::clone(&calls),
        });

        let _node = DeviceNodeSpec::virtual_device(DeviceNodeId::new("counted").unwrap(), model);

        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn firmware_interface_matrix_is_explicit() {
        let fdt = || {
            FdtContributionSpec::Conventional(FdtNodeSpec::new("matrix").with_compatible("test"))
        };
        let acpi = || AcpiContributionSpec::Conventional(AcpiDeviceSpec::new("MTRX", "TEST0001"));

        let none = resolved_firmware(DeviceFirmwareSpec::None);
        assert!(none.validate_fdt_support().is_ok());
        assert!(none.validate_acpi_support().is_ok());

        let fdt_only = resolved_firmware(DeviceFirmwareSpec::interfaces(
            Some(alloc::vec![fdt()]),
            None,
        ));
        assert!(fdt_only.validate_fdt_support().is_ok());
        assert!(fdt_only.validate_acpi_support().is_err());

        let acpi_only = resolved_firmware(DeviceFirmwareSpec::interfaces(
            None,
            Some(alloc::vec![acpi()]),
        ));
        assert!(acpi_only.validate_fdt_support().is_err());
        assert!(acpi_only.validate_acpi_support().is_ok());

        let both = resolved_firmware(DeviceFirmwareSpec::interfaces(
            Some(alloc::vec![fdt()]),
            Some(alloc::vec![acpi()]),
        ));
        assert!(both.validate_fdt_support().is_ok());
        assert!(both.validate_acpi_support().is_ok());
    }

    #[test]
    fn empty_firmware_interfaces_are_rejected_during_declaration() {
        let mut graph = DeviceGraphBuilder::new();
        graph
            .add(DeviceNodeSpec::virtual_device(
                DeviceNodeId::new("invalid-firmware").unwrap(),
                Arc::new(StaticFirmwareModel(DeviceFirmwareSpec::interfaces(
                    None, None,
                ))),
            ))
            .unwrap();

        assert!(matches!(
            graph.declare(),
            Err(DeviceGraphError::Declaration { .. })
        ));
    }
}
