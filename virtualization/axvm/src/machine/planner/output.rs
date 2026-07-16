//! Final resource assignments consumed by VM construction.

use alloc::vec::Vec;

use axdevice::{DeviceBackend, DeviceModelId, ResolvedDeviceResources, ResourceSlot};
use axvm_types::{GuestFirmwareKind, InterruptDelivery, InterruptTriggerMode, VmMachineMode};

use super::super::{
    AddressRange, DeviceDisposition, DeviceInstanceId, HostDeviceDependency, HostDeviceDescriptor,
    HostDeviceId, HostFirmwareActivation, HostInterruptResource, InterruptControllerPlan,
    IoPortRange, LoongArchPlatformPlan,
};

/// A guest interrupt assigned to one named virtual-device resource slot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedInterrupt {
    slot: ResourceSlot,
    id: u32,
    trigger: InterruptTriggerMode,
}

impl ResolvedInterrupt {
    pub(super) const fn new(slot: ResourceSlot, id: u32, trigger: InterruptTriggerMode) -> Self {
        Self { slot, id, trigger }
    }

    /// Returns the model-defined resource name.
    pub const fn slot(&self) -> &ResourceSlot {
        &self.slot
    }

    /// Returns the guest interrupt identifier.
    pub const fn id(&self) -> u32 {
        self.id
    }

    /// Returns the required trigger mode.
    pub const fn trigger(&self) -> InterruptTriggerMode {
        self.trigger
    }
}

/// A guest MMIO window assigned to one named virtual-device resource slot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedMmio {
    slot: ResourceSlot,
    range: AddressRange,
}

impl ResolvedMmio {
    pub(super) const fn new(slot: ResourceSlot, range: AddressRange) -> Self {
        Self { slot, range }
    }

    /// Returns the model-defined resource name.
    pub const fn slot(&self) -> &ResourceSlot {
        &self.slot
    }

    /// Returns the assigned guest address range.
    pub const fn range(&self) -> AddressRange {
        self.range
    }
}

/// A guest port range assigned to one named virtual-device resource slot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedPio {
    slot: ResourceSlot,
    range: IoPortRange,
}

impl ResolvedPio {
    pub(super) const fn new(slot: ResourceSlot, range: IoPortRange) -> Self {
        Self { slot, range }
    }

    /// Returns the model-defined resource name.
    pub const fn slot(&self) -> &ResourceSlot {
        &self.slot
    }

    /// Returns the assigned guest port range.
    pub const fn range(&self) -> IoPortRange {
        self.range
    }
}

/// Resources resolved for one virtual device instance.
#[derive(Clone, Debug)]
pub struct ResolvedVirtualDevice {
    instance_id: DeviceInstanceId,
    model_id: DeviceModelId,
    host_template: Option<HostDeviceId>,
    mmio: Vec<ResolvedMmio>,
    pio: Vec<ResolvedPio>,
    interrupts: Vec<ResolvedInterrupt>,
    resources: ResolvedDeviceResources,
    backend: DeviceBackend,
}

pub(super) struct ResolvedVirtualDeviceParts {
    pub(super) instance_id: DeviceInstanceId,
    pub(super) model_id: DeviceModelId,
    pub(super) host_template: Option<HostDeviceId>,
    pub(super) mmio: Vec<ResolvedMmio>,
    pub(super) pio: Vec<ResolvedPio>,
    pub(super) interrupts: Vec<ResolvedInterrupt>,
    pub(super) resources: ResolvedDeviceResources,
    pub(super) backend: DeviceBackend,
}

impl ResolvedVirtualDevice {
    pub(super) fn from_parts(parts: ResolvedVirtualDeviceParts) -> Self {
        Self {
            instance_id: parts.instance_id,
            model_id: parts.model_id,
            host_template: parts.host_template,
            mmio: parts.mmio,
            pio: parts.pio,
            interrupts: parts.interrupts,
            resources: parts.resources,
            backend: parts.backend,
        }
    }

    /// Returns the stable virtual device instance identity.
    pub const fn instance_id(&self) -> &DeviceInstanceId {
        &self.instance_id
    }

    /// Returns the selected virtual device model.
    pub const fn model_id(&self) -> &DeviceModelId {
        &self.model_id
    }

    /// Returns the host firmware template, if one was consumed.
    pub const fn host_template(&self) -> Option<&HostDeviceId> {
        self.host_template.as_ref()
    }

    /// Returns resolved guest MMIO windows.
    pub fn mmio(&self) -> &[ResolvedMmio] {
        &self.mmio
    }

    /// Returns resolved guest port-I/O ranges.
    pub fn pio(&self) -> &[ResolvedPio] {
        &self.pio
    }

    /// Returns resolved guest interrupt inputs.
    pub fn interrupts(&self) -> &[ResolvedInterrupt] {
        &self.interrupts
    }

    /// Returns the same named resources consumed by the model build phase.
    pub const fn resources(&self) -> &ResolvedDeviceResources {
        &self.resources
    }

    /// Returns the external backend capability selected for this instance.
    pub const fn backend(&self) -> DeviceBackend {
        self.backend
    }
}

/// Host-device disposition recorded in a final plan.
#[derive(Clone, Debug)]
pub struct PlannedHostDevice {
    descriptor: HostDeviceDescriptor,
    disposition: DeviceDisposition,
}

impl PlannedHostDevice {
    pub(super) const fn new(
        descriptor: HostDeviceDescriptor,
        disposition: DeviceDisposition,
    ) -> Self {
        Self {
            descriptor,
            disposition,
        }
    }

    /// Returns the host device identity.
    pub const fn id(&self) -> &HostDeviceId {
        self.descriptor.id()
    }

    /// Returns the selected physical-device disposition.
    pub const fn disposition(&self) -> DeviceDisposition {
        self.disposition
    }

    /// Returns how assignment affects the source firmware activation state.
    pub const fn firmware_activation(&self) -> HostFirmwareActivation {
        self.descriptor.firmware_activation()
    }

    /// Returns the final host MMIO resources associated with this device.
    pub fn mmio(&self) -> &[AddressRange] {
        self.descriptor.mmio()
    }

    /// Returns final host port-I/O resources associated with this device.
    pub fn pio(&self) -> &[IoPortRange] {
        self.descriptor.pio()
    }

    /// Returns complete platform interrupt identifiers associated with this device.
    pub fn interrupts(&self) -> &[HostInterruptResource] {
        self.descriptor.interrupts()
    }

    /// Returns firmware provider dependencies associated with this device.
    pub fn dependencies(&self) -> &[HostDeviceDependency] {
        self.descriptor.dependencies()
    }

    /// Returns firmware-compatible identifiers in source order.
    pub fn compatibles(&self) -> &[alloc::string::String] {
        self.descriptor.compatibles()
    }

    pub(super) const fn set_disposition(&mut self, disposition: DeviceDisposition) {
        self.disposition = disposition;
    }
}

/// Complete deterministic result consumed by VM construction.
#[derive(Clone, Debug)]
pub struct VmMachinePlan {
    snapshot_generation: u64,
    host_console: Option<HostDeviceId>,
    mode: VmMachineMode,
    firmware: GuestFirmwareKind,
    interrupt_delivery: InterruptDelivery,
    interrupt_controller: Option<InterruptControllerPlan>,
    loongarch_platform: Option<LoongArchPlatformPlan>,
    guest_memory: Vec<AddressRange>,
    identity_mappings: Vec<AddressRange>,
    virtual_devices: Vec<ResolvedVirtualDevice>,
    host_devices: Vec<PlannedHostDevice>,
    assigned_host_interrupts: Vec<HostInterruptResource>,
    claims: Vec<HostDeviceId>,
    generated_firmware: Option<GeneratedFirmware>,
}

pub(super) struct VmMachinePlanParts {
    pub(super) snapshot_generation: u64,
    pub(super) host_console: Option<HostDeviceId>,
    pub(super) mode: VmMachineMode,
    pub(super) firmware: GuestFirmwareKind,
    pub(super) interrupt_delivery: InterruptDelivery,
    pub(super) interrupt_controller: Option<InterruptControllerPlan>,
    pub(super) loongarch_platform: Option<LoongArchPlatformPlan>,
    pub(super) guest_memory: Vec<AddressRange>,
    pub(super) identity_mappings: Vec<AddressRange>,
    pub(super) virtual_devices: Vec<ResolvedVirtualDevice>,
    pub(super) host_devices: Vec<PlannedHostDevice>,
    pub(super) assigned_host_interrupts: Vec<HostInterruptResource>,
    pub(super) claims: Vec<HostDeviceId>,
}

/// Final firmware representation produced from resolved machine resources.
#[derive(Clone, Debug)]
pub enum GeneratedFirmware {
    /// A flattened device tree loaded at the configured DTB address.
    DeviceTree(Vec<u8>),
    /// Address-resolved ACPI tables whose image begins at the RSDP.
    Acpi(super::super::GeneratedAcpiImage),
    /// Relocatable ACPI files installed by a fw_cfg-aware guest firmware.
    FwCfgAcpi(axdevice::FwCfgAcpiFiles),
}

impl Default for VmMachinePlan {
    fn default() -> Self {
        Self::empty(
            VmMachineMode::Virtual,
            GuestFirmwareKind::Auto,
            InterruptDelivery::Mediated,
        )
    }
}

impl VmMachinePlan {
    pub(super) fn from_parts(parts: VmMachinePlanParts) -> Self {
        Self {
            snapshot_generation: parts.snapshot_generation,
            host_console: parts.host_console,
            mode: parts.mode,
            firmware: parts.firmware,
            interrupt_delivery: parts.interrupt_delivery,
            interrupt_controller: parts.interrupt_controller,
            loongarch_platform: parts.loongarch_platform,
            guest_memory: parts.guest_memory,
            identity_mappings: parts.identity_mappings,
            virtual_devices: parts.virtual_devices,
            host_devices: parts.host_devices,
            assigned_host_interrupts: parts.assigned_host_interrupts,
            claims: parts.claims,
            generated_firmware: None,
        }
    }

    /// Creates an empty plan for architecture-owned tests and infrastructure.
    pub const fn empty(
        mode: VmMachineMode,
        firmware: GuestFirmwareKind,
        interrupt_delivery: InterruptDelivery,
    ) -> Self {
        Self {
            snapshot_generation: 0,
            host_console: None,
            mode,
            firmware,
            interrupt_delivery,
            interrupt_controller: None,
            loongarch_platform: None,
            guest_memory: Vec::new(),
            identity_mappings: Vec::new(),
            virtual_devices: Vec::new(),
            host_devices: Vec::new(),
            assigned_host_interrupts: Vec::new(),
            claims: Vec::new(),
            generated_firmware: None,
        }
    }

    /// Returns the host snapshot generation that must be revalidated at commit.
    pub const fn snapshot_generation(&self) -> u64 {
        self.snapshot_generation
    }

    /// Returns the physical device selected for host console I/O.
    pub const fn host_console(&self) -> Option<&HostDeviceId> {
        self.host_console.as_ref()
    }

    /// Returns the machine construction mode.
    pub const fn mode(&self) -> VmMachineMode {
        self.mode
    }

    /// Returns the selected guest firmware description.
    pub const fn firmware(&self) -> GuestFirmwareKind {
        self.firmware
    }

    /// Returns normalized interrupt delivery.
    pub const fn interrupt_delivery(&self) -> InterruptDelivery {
        self.interrupt_delivery
    }

    /// Returns the controller topology selected by the architecture profile.
    pub const fn interrupt_controller(&self) -> Option<&InterruptControllerPlan> {
        self.interrupt_controller.as_ref()
    }

    /// Returns finalized LoongArch firmware-facing platform resources.
    pub const fn loongarch_platform(&self) -> Option<&LoongArchPlatformPlan> {
        self.loongarch_platform.as_ref()
    }

    /// Returns explicit guest RAM and shared-memory address ranges.
    pub fn guest_memory(&self) -> &[AddressRange] {
        &self.guest_memory
    }

    /// Returns final non-overlapping identity-mapped I/O ranges.
    pub fn identity_mappings(&self) -> &[AddressRange] {
        &self.identity_mappings
    }

    /// Returns virtual devices sorted by stable instance identity.
    pub fn virtual_devices(&self) -> &[ResolvedVirtualDevice] {
        &self.virtual_devices
    }

    /// Returns physical-device dispositions in host firmware order.
    pub fn host_devices(&self) -> &[PlannedHostDevice] {
        &self.host_devices
    }

    /// Returns unique physical interrupt routes owned by passthrough devices.
    pub fn assigned_host_interrupts(&self) -> &[HostInterruptResource] {
        &self.assigned_host_interrupts
    }

    /// Iterates port-I/O ranges owned by passthrough devices.
    pub fn assigned_host_pio(&self) -> impl Iterator<Item = IoPortRange> + '_ {
        self.host_devices
            .iter()
            .filter(|device| device.disposition == DeviceDisposition::Passthrough)
            .flat_map(|device| device.pio().iter().copied())
    }

    /// Returns devices that must be claimed transactionally before commit.
    pub fn claims(&self) -> &[HostDeviceId] {
        &self.claims
    }

    /// Attaches a final generated device tree.
    pub fn with_device_tree_firmware(mut self, bytes: Vec<u8>) -> Self {
        self.generated_firmware = Some(GeneratedFirmware::DeviceTree(bytes));
        self
    }

    /// Attaches a final generated ACPI image.
    pub fn with_acpi_firmware(mut self, image: super::super::GeneratedAcpiImage) -> Self {
        self.generated_firmware = Some(GeneratedFirmware::Acpi(image));
        self
    }

    /// Returns the generated device tree, if selected.
    pub fn device_tree_firmware(&self) -> Option<&[u8]> {
        match self.generated_firmware.as_ref() {
            Some(GeneratedFirmware::DeviceTree(bytes)) => Some(bytes),
            _ => None,
        }
    }

    /// Returns the generated ACPI image, if selected.
    pub const fn acpi_firmware(&self) -> Option<&super::super::GeneratedAcpiImage> {
        match self.generated_firmware.as_ref() {
            Some(GeneratedFirmware::Acpi(image)) => Some(image),
            _ => None,
        }
    }

    /// Returns relocatable fw_cfg ACPI files, if selected.
    pub const fn fw_cfg_acpi_firmware(&self) -> Option<&axdevice::FwCfgAcpiFiles> {
        match self.generated_firmware.as_ref() {
            Some(GeneratedFirmware::FwCfgAcpi(files)) => Some(files),
            _ => None,
        }
    }

    /// Attaches relocatable ACPI files for a fw_cfg-aware guest firmware.
    pub fn with_fw_cfg_acpi_firmware(mut self, files: axdevice::FwCfgAcpiFiles) -> Self {
        self.generated_firmware = Some(GeneratedFirmware::FwCfgAcpi(files));
        self
    }
}
