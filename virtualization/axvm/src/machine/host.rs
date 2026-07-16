//! Normalized host-platform devices and ownership.

use alloc::{string::String, vec::Vec};

use axdevice_base::ControllerInputId;
use axvm_types::InterruptTriggerMode;

use super::{AddressRange, HostDeviceId, IoPortRange, MachinePlanResult, selector_label};

/// Host ownership state relevant to VM assignment.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostDeviceOwnership {
    /// The host or hypervisor retains the physical device permanently.
    HostExclusive,
    /// The host can release the device during a VM build transaction.
    Transferable,
    /// The device is available for immediate assignment.
    Assignable,
    /// The node carries firmware structure but owns no guest-accessible device.
    Structural,
    /// The device cannot be represented or isolated safely.
    Unrepresentable,
}

/// Trusted evidence used to transfer the active host console into a VM plan.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostConsoleEvidence {
    /// The active console is selected by usable host firmware metadata.
    Firmware,
    /// A live platform capability identifies the device currently in use.
    LivePlatform,
}

/// Stable location used to identify a host console in a platform snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HostConsoleLocation {
    /// Exact firmware identity of a probed console device.
    Device(HostDeviceId),
    /// Physical base of the active boot-console MMIO aperture.
    MmioBase(u64),
}

impl HostConsoleEvidence {
    fn accepts(self, ownership: HostDeviceOwnership) -> bool {
        match self {
            Self::Firmware => !matches!(
                ownership,
                HostDeviceOwnership::Structural | HostDeviceOwnership::Unrepresentable
            ),
            Self::LivePlatform => ownership != HostDeviceOwnership::Structural,
        }
    }
}

/// Whether a firmware dependency is necessary to expose a physical device.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostDeviceDependencyKind {
    /// The consumer cannot be represented when the provider is unavailable.
    Required,
    /// The capability may be omitted while preserving a safe device model.
    Optional,
}

/// One firmware dependency from a host device to a provider node.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostDeviceDependency {
    provider: HostDeviceId,
    property: String,
    kind: HostDeviceDependencyKind,
}

impl HostDeviceDependency {
    /// Creates a checked firmware dependency.
    pub fn new(
        provider: HostDeviceId,
        property: impl Into<String>,
        kind: HostDeviceDependencyKind,
    ) -> MachinePlanResult<Self> {
        let property = property.into();
        if property.trim().is_empty() {
            return Err(super::MachinePlanError::EmptyIdentifier {
                kind: "host device dependency property",
            });
        }
        Ok(Self {
            provider,
            property,
            kind,
        })
    }

    /// Returns the stable identity of the provider node.
    pub const fn provider(&self) -> &HostDeviceId {
        &self.provider
    }

    /// Returns the firmware property containing the reference.
    pub fn property(&self) -> &str {
        &self.property
    }

    /// Returns whether the provider is required or optional.
    pub const fn kind(&self) -> HostDeviceDependencyKind {
        self.kind
    }
}

/// Firmware description of the host-side route feeding one controller input.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HostInterruptSource {
    /// A controller-local input supplied by trusted programmatic platform data.
    ControllerInput,
    /// A raw FDT interrupt specifier and the controller node that owns it.
    Fdt {
        /// Stable firmware identity of the interrupt controller.
        controller: HostDeviceId,
        /// Unmodified cells from the device's `interrupts` property.
        specifier: Vec<u32>,
    },
    /// A complete ACPI GSI route, including controller, trigger, and polarity.
    AcpiGsiRoute(irq_framework::AcpiGsiRoute),
}

/// One physical-device interrupt kept separate from its guest controller input.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostInterruptResource {
    input: u32,
    trigger: InterruptTriggerMode,
    source: HostInterruptSource,
}

impl HostInterruptResource {
    /// Creates a trusted controller-local interrupt resource.
    pub const fn controller_input(input: u32, trigger: InterruptTriggerMode) -> Self {
        Self {
            input,
            trigger,
            source: HostInterruptSource::ControllerInput,
        }
    }

    /// Creates an interrupt resource from a validated FDT binding.
    pub fn fdt(
        input: u32,
        trigger: InterruptTriggerMode,
        controller: HostDeviceId,
        specifier: Vec<u32>,
    ) -> MachinePlanResult<Self> {
        if specifier.is_empty() {
            return Err(super::MachinePlanError::InvalidFirmware {
                detail: alloc::format!(
                    "FDT interrupt input {input} from controller '{controller}' has no specifier"
                ),
            });
        }
        Ok(Self {
            input,
            trigger,
            source: HostInterruptSource::Fdt {
                controller,
                specifier,
            },
        })
    }

    /// Creates an interrupt resource from complete ACPI routing metadata.
    pub const fn acpi(route: irq_framework::AcpiGsiRoute) -> Self {
        Self::routed_acpi(route.gsi, route)
    }

    /// Creates an interrupt resource whose guest controller input differs from
    /// the host ACPI GSI route.
    pub const fn routed_acpi(input: u32, route: irq_framework::AcpiGsiRoute) -> Self {
        let trigger = match route.trigger {
            irq_framework::AcpiIrqTrigger::Edge => InterruptTriggerMode::EdgeTriggered,
            irq_framework::AcpiIrqTrigger::Level => InterruptTriggerMode::LevelTriggered,
        };
        Self {
            input,
            trigger,
            source: HostInterruptSource::AcpiGsiRoute(route),
        }
    }

    /// Returns the guest controller input used by identity-described devices.
    pub const fn input(&self) -> ControllerInputId {
        ControllerInputId::new(self.input as usize)
    }

    /// Returns the firmware-visible input number.
    pub const fn input_u32(&self) -> u32 {
        self.input
    }

    /// Returns the electrical trigger semantics.
    pub const fn trigger(&self) -> InterruptTriggerMode {
        self.trigger
    }

    /// Returns the host firmware route used to resolve the physical IRQ.
    pub const fn source(&self) -> &HostInterruptSource {
        &self.source
    }
}

/// Final physical-device disposition selected by the planner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeviceDisposition {
    /// The physical device remains owned by the host.
    HostExclusive,
    /// VM policy denied the physical device.
    Denied,
    /// A virtual device replaces the physical aperture.
    VirtualReplacement,
    /// The physical device is assigned to the VM.
    Passthrough,
    /// The node is retained only as firmware structure.
    Structural,
    /// The device cannot be represented safely.
    Unrepresentable,
}

/// Selector used by VM policy or an explicit virtual-device template request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HostDeviceSelector {
    /// Select one exact stable host device identity.
    Id(HostDeviceId),
    /// Select a firmware path and all descendants.
    PathSubtree(HostDeviceId),
    /// Select devices advertising one compatible identifier.
    Compatible(String),
    /// Deny one raw MMIO range even when firmware has no device node.
    Mmio(AddressRange),
    /// Deny one raw interrupt identifier.
    Interrupt(ControllerInputId),
}

impl HostDeviceSelector {
    /// Creates a compatible-string selector.
    pub fn compatible(value: impl Into<String>) -> MachinePlanResult<Self> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(super::MachinePlanError::EmptyIdentifier { kind: "compatible" });
        }
        Ok(Self::Compatible(value))
    }

    pub(crate) fn matches(&self, device: &HostDeviceDescriptor) -> bool {
        match self {
            Self::Id(id) => device.id() == id,
            Self::PathSubtree(path) => {
                device.id().as_str() == path.as_str()
                    || device
                        .id()
                        .as_str()
                        .strip_prefix(path.as_str())
                        .is_some_and(|suffix| suffix.starts_with('/'))
            }
            Self::Compatible(compatible) => device
                .compatibles()
                .iter()
                .any(|candidate| candidate == compatible),
            Self::Mmio(_) | Self::Interrupt(_) => false,
        }
    }

    pub(crate) fn label(&self) -> String {
        match self {
            Self::Id(id) => selector_label("id", id),
            Self::PathSubtree(path) => selector_label("subtree", path),
            Self::Compatible(value) => selector_label("compatible", value),
            Self::Mmio(range) => alloc::format!("mmio:{:#x}..{:#x}", range.base(), range.end()),
            Self::Interrupt(interrupt) => selector_label("interrupt", interrupt.value()),
        }
    }
}

/// One host firmware device normalized for VM planning.
#[derive(Clone, Debug)]
pub struct HostDeviceDescriptor {
    id: HostDeviceId,
    ownership: HostDeviceOwnership,
    compatibles: Vec<String>,
    mmio: Vec<AddressRange>,
    pio: Vec<IoPortRange>,
    interrupts: Vec<HostInterruptResource>,
    dependencies: Vec<HostDeviceDependency>,
}

impl HostDeviceDescriptor {
    /// Creates a host device descriptor with no resources.
    pub fn new(id: HostDeviceId, ownership: HostDeviceOwnership) -> Self {
        Self {
            id,
            ownership,
            compatibles: Vec::new(),
            mmio: Vec::new(),
            pio: Vec::new(),
            interrupts: Vec::new(),
            dependencies: Vec::new(),
        }
    }

    /// Adds a firmware compatible identifier.
    pub fn with_compatible(mut self, compatible: impl Into<String>) -> Self {
        self.compatibles.push(compatible.into());
        self
    }

    /// Adds a host MMIO resource.
    pub fn with_mmio(mut self, range: AddressRange) -> Self {
        self.mmio.push(range);
        self
    }

    /// Adds a host port-I/O resource.
    pub fn with_pio(mut self, range: IoPortRange) -> Self {
        self.pio.push(range);
        self
    }

    /// Adds a host interrupt resource.
    pub fn with_interrupt(mut self, interrupt: HostInterruptResource) -> Self {
        self.interrupts.push(interrupt);
        self
    }

    /// Adds a firmware dependency on another host node.
    pub fn with_dependency(mut self, dependency: HostDeviceDependency) -> Self {
        self.dependencies.push(dependency);
        self
    }

    /// Returns the stable host device identity.
    pub const fn id(&self) -> &HostDeviceId {
        &self.id
    }

    /// Returns the current ownership policy.
    pub const fn ownership(&self) -> HostDeviceOwnership {
        self.ownership
    }

    /// Returns firmware compatible identifiers in source order.
    pub fn compatibles(&self) -> &[String] {
        &self.compatibles
    }

    /// Returns MMIO resources in firmware order.
    pub fn mmio(&self) -> &[AddressRange] {
        &self.mmio
    }

    /// Returns port-I/O resources in firmware order.
    pub fn pio(&self) -> &[IoPortRange] {
        &self.pio
    }

    /// Returns interrupt resources in firmware order.
    pub fn interrupts(&self) -> &[HostInterruptResource] {
        &self.interrupts
    }

    /// Returns firmware dependencies in source-property order.
    pub fn dependencies(&self) -> &[HostDeviceDependency] {
        &self.dependencies
    }

    pub(crate) fn set_ownership(&mut self, ownership: HostDeviceOwnership) {
        self.ownership = ownership;
    }
}

pub(crate) fn is_guest_firmware_infrastructure(compatibles: &[String]) -> bool {
    compatibles.iter().any(|compatible| {
        matches!(
            compatible.as_str(),
            "arm,gic-v3" | "arm,gic-v3-its" | "arm,armv8-timer" | "arm,psci-1.0" | "arm,psci-0.2"
        ) || compatible.starts_with("riscv,plic")
            || compatible.starts_with("riscv,cpu-intc")
    })
}

/// Immutable platform snapshot consumed by one planning attempt.
#[derive(Clone, Debug)]
pub struct HostPlatformSnapshot {
    generation: u64,
    io_apertures: Vec<AddressRange>,
    devices: Vec<HostDeviceDescriptor>,
    console_device: Option<HostDeviceId>,
    source_fdt: Option<Vec<u8>>,
}

impl HostPlatformSnapshot {
    /// Creates an empty snapshot with a platform generation token.
    pub const fn new(generation: u64) -> Self {
        Self {
            generation,
            io_apertures: Vec::new(),
            devices: Vec::new(),
            console_device: None,
            source_fdt: None,
        }
    }

    /// Adds a non-RAM host I/O aperture.
    pub fn with_io_aperture(mut self, range: AddressRange) -> Self {
        self.io_apertures.push(range);
        self
    }

    /// Adds one host device while preserving firmware traversal order.
    pub fn with_device(mut self, device: HostDeviceDescriptor) -> Self {
        self.devices.push(device);
        self
    }

    /// Selects the host device used for platform console input and output.
    ///
    /// # Errors
    ///
    /// Returns an error if the device has not been added to this snapshot.
    pub fn with_console_device(mut self, device: HostDeviceId) -> MachinePlanResult<Self> {
        self.set_console_device(device)?;
        Ok(self)
    }

    /// Returns the generation used to revalidate a later claim transaction.
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// Returns non-RAM host I/O apertures.
    pub fn io_apertures(&self) -> &[AddressRange] {
        &self.io_apertures
    }

    /// Returns host devices in stable firmware order.
    pub fn devices(&self) -> &[HostDeviceDescriptor] {
        &self.devices
    }

    /// Returns the physical device selected for host console I/O.
    pub const fn console_device(&self) -> Option<&HostDeviceId> {
        self.console_device.as_ref()
    }

    /// Returns the immutable host FDT backing this snapshot, when available.
    pub fn source_fdt(&self) -> Option<&[u8]> {
        self.source_fdt.as_deref()
    }

    pub(crate) fn set_source_fdt(&mut self, bytes: &[u8]) {
        self.source_fdt = Some(bytes.to_vec());
    }

    pub(crate) fn set_console_device(&mut self, device: HostDeviceId) -> MachinePlanResult<()> {
        if !self
            .devices
            .iter()
            .any(|candidate| candidate.id() == &device)
        {
            return Err(super::MachinePlanError::InvalidFirmware {
                detail: alloc::format!(
                    "host console device '{device}' is absent from the platform snapshot"
                ),
            });
        }
        self.console_device = Some(device);
        Ok(())
    }

    /// Grants transfer of the active host console using trusted evidence.
    ///
    /// Live platform evidence intentionally supersedes conservative firmware
    /// classification, including a stale `status = "disabled"`. Structural
    /// nodes remain non-transferable with either evidence source.
    ///
    /// # Errors
    ///
    /// Returns an error if the device is absent or its ownership state cannot
    /// be transferred using the supplied evidence.
    pub fn grant_console_transfer(
        &mut self,
        location: HostConsoleLocation,
        evidence: HostConsoleEvidence,
    ) -> MachinePlanResult<()> {
        let device = self.resolve_console_location(location)?;
        let descriptor = self
            .devices
            .iter_mut()
            .find(|candidate| candidate.id() == &device)
            .ok_or_else(|| super::MachinePlanError::InvalidFirmware {
                detail: alloc::format!(
                    "transferable host console '{device}' is absent from the platform snapshot"
                ),
            })?;
        if !evidence.accepts(descriptor.ownership()) {
            return Err(super::MachinePlanError::InvalidFirmware {
                detail: alloc::format!(
                    "host console '{device}' cannot be transferred from ownership state {:?}",
                    descriptor.ownership()
                ),
            });
        }
        descriptor.set_ownership(HostDeviceOwnership::Transferable);
        self.console_device = Some(device);
        Ok(())
    }

    fn resolve_console_location(
        &self,
        location: HostConsoleLocation,
    ) -> MachinePlanResult<HostDeviceId> {
        let base = match location {
            HostConsoleLocation::Device(device) => return Ok(device),
            HostConsoleLocation::MmioBase(base) => base,
        };
        let mut matches = self
            .devices
            .iter()
            .filter(|device| device.mmio().iter().any(|range| range.base() == base));
        let first = matches
            .next()
            .ok_or_else(|| super::MachinePlanError::InvalidFirmware {
                detail: alloc::format!(
                    "active host console at MMIO base {base:#x} is absent from the platform \
                     snapshot"
                ),
            })?;
        if let Some(second) = matches.next() {
            return Err(super::MachinePlanError::InvalidFirmware {
                detail: alloc::format!(
                    "active host console MMIO base {base:#x} is shared by '{}' and '{}'",
                    first.id(),
                    second.id()
                ),
            });
        }
        Ok(first.id().clone())
    }
}
