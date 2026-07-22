//! Normalized host-platform devices and ownership.

use alloc::{
    string::{String, ToString},
    vec::Vec,
};

use axdevice_base::ControllerInputId;
use axvm_types::InterruptTriggerMode;

use super::{
    AddressRange, HostDeviceDependency, HostDeviceId, HostProviderResourceGrant, IoPortRange,
    MachinePlanResult, selector_label,
};

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
    /// Firmware describes an inactive alternative that owns no live resource.
    Inactive,
    /// The device cannot be represented or isolated safely.
    Unrepresentable,
}

/// Trusted authority that permits physical resources to enter a VM plan.
///
/// Firmware describes hardware, but does not prove that the host has stopped
/// using it. Assignment authority therefore comes from a live platform
/// capability or from a static partition description, never from FDT/ACPI
/// classification alone.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostDeviceAssignment {
    /// The device belongs to a static partition and has no live host state to
    /// suspend before assignment.
    StaticPartition,
    /// A live host capability can suspend the device and restore its complete
    /// state when the VM lease is dropped.
    ReversibleTransfer,
}

impl HostDeviceAssignment {
    const fn accepts(self, ownership: HostDeviceOwnership) -> bool {
        matches!(
            (self, ownership),
            (
                Self::StaticPartition,
                HostDeviceOwnership::Assignable | HostDeviceOwnership::Structural
            ) | (Self::ReversibleTransfer, HostDeviceOwnership::Transferable)
        )
    }
}

/// How assigning a host device affects its source firmware activation state.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum HostFirmwareActivation {
    /// Keep the status represented by the captured host firmware.
    Preserve,
    /// Mark the device available when materializing guest firmware.
    #[default]
    Enable,
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
                HostDeviceOwnership::Structural
                    | HostDeviceOwnership::Inactive
                    | HostDeviceOwnership::Unrepresentable
            ),
            Self::LivePlatform => ownership != HostDeviceOwnership::Structural,
        }
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
    /// The inactive firmware alternative is omitted without claiming or
    /// protecting its physical resource aliases.
    Inactive,
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
    assignment: Option<HostDeviceAssignment>,
    firmware_activation: HostFirmwareActivation,
    compatibles: Vec<String>,
    mmio: Vec<AddressRange>,
    pio: Vec<IoPortRange>,
    interrupts: Vec<HostInterruptResource>,
    dependencies: Vec<HostDeviceDependency>,
    provider_resources: Vec<HostProviderResourceGrant>,
}

impl HostDeviceDescriptor {
    /// Creates a descriptor supplied by trusted programmatic platform data.
    ///
    /// `Assignable` devices receive static-partition authority and
    /// `Transferable` devices receive reversible-transfer authority. Firmware
    /// parsers use a private descriptive constructor so parsing a node never
    /// grants assignment implicitly.
    pub fn new(id: HostDeviceId, ownership: HostDeviceOwnership) -> Self {
        let assignment = match ownership {
            HostDeviceOwnership::Assignable => Some(HostDeviceAssignment::StaticPartition),
            HostDeviceOwnership::Transferable => Some(HostDeviceAssignment::ReversibleTransfer),
            _ => None,
        };
        Self {
            id,
            ownership,
            assignment,
            firmware_activation: HostFirmwareActivation::Enable,
            compatibles: Vec::new(),
            mmio: Vec::new(),
            pio: Vec::new(),
            interrupts: Vec::new(),
            dependencies: Vec::new(),
            provider_resources: Vec::new(),
        }
    }

    pub(crate) fn described(id: HostDeviceId, ownership: HostDeviceOwnership) -> Self {
        let mut descriptor = Self::new(id, ownership);
        descriptor.assignment = None;
        descriptor
    }

    /// Attaches trusted assignment authority to a descriptor.
    ///
    /// # Errors
    ///
    /// Returns an error when the authority contradicts the descriptor's
    /// ownership classification.
    pub fn with_assignment(mut self, assignment: HostDeviceAssignment) -> MachinePlanResult<Self> {
        self.set_assignment(assignment)?;
        Ok(self)
    }

    /// Selects how guest firmware materialization treats the source status.
    pub fn with_firmware_activation(mut self, activation: HostFirmwareActivation) -> Self {
        self.firmware_activation = activation;
        self
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

    /// Returns the trusted authority permitting physical assignment.
    pub const fn assignment(&self) -> Option<HostDeviceAssignment> {
        self.assignment
    }

    /// Returns whether this descriptor owns a physical resource that must be
    /// mapped, routed, or leased before guest use.
    pub fn has_physical_resources(&self) -> bool {
        !self.mmio.is_empty() || !self.pio.is_empty() || !self.interrupts.is_empty()
    }

    /// Returns how guest firmware materialization treats the source status.
    pub const fn firmware_activation(&self) -> HostFirmwareActivation {
        self.firmware_activation
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

    /// Returns static provider-local resources granted by the platform.
    pub fn provider_resources(&self) -> &[HostProviderResourceGrant] {
        &self.provider_resources
    }

    fn grant_provider_resource(
        &mut self,
        grant: HostProviderResourceGrant,
    ) -> MachinePlanResult<bool> {
        if let Some(existing) = self.provider_resources.iter().find(|existing| {
            existing.reference().kind() == grant.reference().kind()
                && existing.reference().specifier() == grant.reference().specifier()
        }) {
            if existing == &grant {
                return Ok(false);
            }
            return Err(super::MachinePlanError::InvalidFirmware {
                detail: alloc::format!(
                    "host provider '{}' has conflicting grants for {:?} selector {:?}",
                    self.id,
                    grant.reference().kind(),
                    grant.reference().specifier(),
                ),
            });
        }
        self.provider_resources.push(grant);
        Ok(true)
    }

    pub(crate) fn set_ownership(&mut self, ownership: HostDeviceOwnership) {
        self.ownership = ownership;
    }

    fn set_assignment(&mut self, assignment: HostDeviceAssignment) -> MachinePlanResult<()> {
        if !assignment.accepts(self.ownership) {
            return Err(super::MachinePlanError::InvalidHostDeviceAssignment {
                device: self.id.to_string(),
                ownership: self.ownership,
                assignment,
            });
        }
        self.assignment = Some(assignment);
        Ok(())
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
    ///
    /// Adding a provider-resource grant folds its pinned state into this token,
    /// so a changed clock rate, gate, or reset state rejects the later claim.
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
    /// Live platform evidence intentionally supersedes conservative ownership
    /// classification, including a stale `status = "disabled"`. It does not
    /// change the source firmware activation state: a wrapper such as an
    /// AArch64 FIQ debugger may own an intentionally disabled UART aperture.
    /// Structural nodes remain non-transferable with either evidence source.
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
        descriptor.set_assignment(HostDeviceAssignment::ReversibleTransfer)?;
        self.console_device = Some(device);
        Ok(())
    }

    /// Grants one firmware-described device trusted physical-assignment
    /// authority.
    ///
    /// This method is intended for architecture adapters after they have
    /// matched the descriptor to a live ownership capability or a static
    /// partition manifest.
    pub fn grant_device_assignment(
        &mut self,
        device: &HostDeviceId,
        assignment: HostDeviceAssignment,
    ) -> MachinePlanResult<()> {
        let descriptor = self
            .devices
            .iter_mut()
            .find(|candidate| candidate.id() == device)
            .ok_or_else(|| super::MachinePlanError::InvalidFirmware {
                detail: alloc::format!(
                    "host assignment authority refers to absent device '{device}'"
                ),
            })?;
        descriptor.set_assignment(assignment)
    }

    /// Grants static state for one resource owned by a host provider.
    ///
    /// The caller is a trusted platform adapter and must keep the resource in
    /// the granted state until every VM plan derived from this snapshot has
    /// either been discarded or its device leases have been released.
    ///
    /// # Errors
    ///
    /// Returns an error if the provider is absent or a conflicting grant was
    /// already recorded for the same provider-local selector.
    pub fn grant_provider_resource(
        &mut self,
        provider: &HostDeviceId,
        grant: HostProviderResourceGrant,
    ) -> MachinePlanResult<()> {
        let next_generation = grant.fold_generation(self.generation, provider.as_str());
        let descriptor = self
            .devices
            .iter_mut()
            .find(|candidate| candidate.id() == provider)
            .ok_or_else(|| super::MachinePlanError::InvalidFirmware {
                detail: alloc::format!(
                    "host provider resource grant refers to absent device '{provider}'"
                ),
            })?;
        if descriptor.grant_provider_resource(grant)? {
            self.generation = next_generation;
        }
        Ok(())
    }

    /// Grants all otherwise assignable resources to a trusted whole-machine
    /// partition.
    ///
    /// Architecture adapters use this only when their platform contract gives
    /// the VM the board's remaining hardware wholesale. Host-exclusive and
    /// unrepresentable resources stay protected; inactive alternatives receive
    /// no assignment. Transferable devices still require a reversible lease.
    pub fn grant_whole_machine_assignment(&mut self) -> MachinePlanResult<()> {
        for descriptor in &mut self.devices {
            let assignment = match descriptor.ownership() {
                HostDeviceOwnership::Assignable => HostDeviceAssignment::StaticPartition,
                HostDeviceOwnership::Transferable => HostDeviceAssignment::ReversibleTransfer,
                HostDeviceOwnership::Structural if descriptor.has_physical_resources() => {
                    HostDeviceAssignment::StaticPartition
                }
                _ => continue,
            };
            descriptor.set_assignment(assignment)?;
        }
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
