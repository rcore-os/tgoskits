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
}

/// Immutable platform snapshot consumed by one planning attempt.
#[derive(Clone, Debug)]
pub struct HostPlatformSnapshot {
    generation: u64,
    io_apertures: Vec<AddressRange>,
    devices: Vec<HostDeviceDescriptor>,
    source_fdt: Option<Vec<u8>>,
}

impl HostPlatformSnapshot {
    /// Creates an empty snapshot with a platform generation token.
    pub const fn new(generation: u64) -> Self {
        Self {
            generation,
            io_apertures: Vec::new(),
            devices: Vec::new(),
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

    /// Returns the immutable host FDT backing this snapshot, when available.
    pub fn source_fdt(&self) -> Option<&[u8]> {
        self.source_fdt.as_deref()
    }

    pub(crate) fn set_source_fdt(&mut self, bytes: &[u8]) {
        self.source_fdt = Some(bytes.to_vec());
    }
}
