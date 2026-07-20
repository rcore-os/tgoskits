//! Immutable VM machine requests and virtual-device declarations.

use alloc::{string::String, vec::Vec};
use core::ops::RangeInclusive;

use axdevice::{DeviceBackend, DeviceModelId, DeviceRequirements};
use axvm_types::{GuestFirmwareKind, PhysicalInterruptPolicy, VmMachineMode};

use super::{
    AddressRange, DeviceInstanceId, HostDeviceSelector, IoPortRange, MachinePlanError,
    MachinePlanResult,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GuestMemoryAddress {
    Fixed(AddressRange),
    IdentityAllocated { size: u64 },
}

/// Address placement selected for one guest memory region.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GuestMemoryPlacement {
    /// The guest address is known while the machine is planned.
    Fixed,
    /// The runtime allocator chooses host RAM and uses the same address in the guest.
    IdentityAllocated,
}

/// One checked guest RAM or shared-memory placement requirement.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GuestMemoryRegion {
    address: GuestMemoryAddress,
}

impl GuestMemoryRegion {
    /// Creates a memory region whose guest address is already known.
    pub const fn new(range: AddressRange) -> Self {
        Self {
            address: GuestMemoryAddress::Fixed(range),
        }
    }

    /// Creates VM-owned memory whose final GPA equals its allocator-selected HPA.
    pub fn identity_allocated(size: u64) -> MachinePlanResult<Self> {
        AddressRange::new(0, size)?;
        Ok(Self {
            address: GuestMemoryAddress::IdentityAllocated { size },
        })
    }

    /// Returns how the guest address is selected.
    pub const fn placement(self) -> GuestMemoryPlacement {
        match self.address {
            GuestMemoryAddress::Fixed(_) => GuestMemoryPlacement::Fixed,
            GuestMemoryAddress::IdentityAllocated { .. } => GuestMemoryPlacement::IdentityAllocated,
        }
    }

    /// Returns the guest range when it is known during machine planning.
    pub const fn fixed_range(self) -> Option<AddressRange> {
        match self.address {
            GuestMemoryAddress::Fixed(range) => Some(range),
            GuestMemoryAddress::IdentityAllocated { .. } => None,
        }
    }

    /// Returns the memory size in bytes.
    pub const fn size(self) -> u64 {
        match self.address {
            GuestMemoryAddress::Fixed(range) => range.size(),
            GuestMemoryAddress::IdentityAllocated { size } => size,
        }
    }
}

/// Source policy for one virtual device instance.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum VirtualDeviceSource {
    /// Match a host compatible in passthrough mode, otherwise allocate.
    #[default]
    Auto,
    /// Always allocate new guest resources.
    Allocate,
    /// Use one explicit host firmware template.
    Host(HostDeviceSelector),
}

/// A virtual device already expanded into named model requirements.
#[derive(Clone, Debug)]
pub struct VirtualDeviceDescriptor {
    instance_id: DeviceInstanceId,
    model_id: DeviceModelId,
    source: VirtualDeviceSource,
    compatible_predicates: Vec<String>,
    requirements: DeviceRequirements,
    backend: DeviceBackend,
}

impl VirtualDeviceDescriptor {
    /// Creates a virtual-device declaration from a model's first-phase result.
    pub fn new(
        instance_id: DeviceInstanceId,
        model_id: DeviceModelId,
        requirements: DeviceRequirements,
    ) -> Self {
        Self {
            instance_id,
            model_id,
            source: VirtualDeviceSource::Auto,
            compatible_predicates: Vec::new(),
            requirements,
            backend: DeviceBackend::None,
        }
    }

    /// Adds one compatible string accepted for host-template replacement.
    pub fn with_compatible(mut self, compatible: impl Into<String>) -> Self {
        self.compatible_predicates.push(compatible.into());
        self
    }

    /// Replaces the resource-source policy.
    pub fn with_source(mut self, source: VirtualDeviceSource) -> Self {
        self.source = source;
        self
    }

    /// Selects the external capability granted during the build phase.
    pub fn with_backend(mut self, backend: DeviceBackend) -> Self {
        self.backend = backend;
        self
    }

    pub(crate) const fn instance_id(&self) -> &DeviceInstanceId {
        &self.instance_id
    }

    pub(crate) const fn model_id(&self) -> &DeviceModelId {
        &self.model_id
    }

    pub(crate) const fn source(&self) -> &VirtualDeviceSource {
        &self.source
    }

    pub(crate) fn compatible_predicates(&self) -> &[String] {
        &self.compatible_predicates
    }

    pub(crate) const fn requirements(&self) -> &DeviceRequirements {
        &self.requirements
    }

    pub(crate) const fn backend(&self) -> DeviceBackend {
        self.backend
    }
}

/// Architecture-specific pools available for dynamically allocated devices.
#[derive(Clone, Debug)]
pub struct MachineProfile {
    mmio_pool: AddressRange,
    interrupt_pool: RangeInclusive<u32>,
    pio_pool: Option<IoPortRange>,
    reserved_mmio: Vec<AddressRange>,
    reserved_interrupts: Vec<u32>,
    interrupt_controller: Option<super::InterruptControllerProfile>,
    loongarch_platform: Option<super::LoongArchPlatformProfile>,
}

impl MachineProfile {
    /// Creates checked MMIO and interrupt pools.
    pub fn new(
        mmio_pool: AddressRange,
        interrupt_pool: RangeInclusive<u32>,
    ) -> MachinePlanResult<Self> {
        if interrupt_pool.start() > interrupt_pool.end() {
            return Err(MachinePlanError::InvalidInterruptPool {
                start: *interrupt_pool.start(),
                end: *interrupt_pool.end(),
            });
        }
        Ok(Self {
            mmio_pool,
            interrupt_pool,
            pio_pool: None,
            reserved_mmio: Vec::new(),
            reserved_interrupts: Vec::new(),
            interrupt_controller: None,
            loongarch_platform: None,
        })
    }

    /// Reserves one profile-owned MMIO range from dynamic allocation.
    pub fn with_reserved_mmio(mut self, range: AddressRange) -> Self {
        self.reserved_mmio.push(range);
        self
    }

    /// Adds an architecture port-I/O pool for dynamically allocated devices.
    pub fn with_pio_pool(mut self, pool: IoPortRange) -> Self {
        self.pio_pool = Some(pool);
        self
    }

    /// Reserves one profile-owned interrupt ID from dynamic allocation.
    pub fn with_reserved_interrupt(mut self, interrupt: u32) -> Self {
        self.reserved_interrupts.push(interrupt);
        self
    }

    /// Selects the mandatory controller topology for this architecture profile.
    pub fn with_interrupt_controller(
        mut self,
        controller: super::InterruptControllerProfile,
    ) -> Self {
        self.interrupt_controller = Some(controller);
        self
    }

    /// Selects firmware-facing LoongArch platform resources.
    pub fn with_loongarch_platform(mut self, platform: super::LoongArchPlatformProfile) -> Self {
        self.loongarch_platform = Some(platform);
        self
    }

    pub(crate) const fn mmio_pool(&self) -> AddressRange {
        self.mmio_pool
    }

    pub(crate) const fn interrupt_pool(&self) -> &RangeInclusive<u32> {
        &self.interrupt_pool
    }

    pub(crate) const fn pio_pool(&self) -> Option<IoPortRange> {
        self.pio_pool
    }

    pub(crate) fn reserved_mmio(&self) -> &[AddressRange] {
        &self.reserved_mmio
    }

    pub(crate) fn reserved_interrupts(&self) -> &[u32] {
        &self.reserved_interrupts
    }

    pub(crate) const fn interrupt_controller(&self) -> Option<&super::InterruptControllerProfile> {
        self.interrupt_controller.as_ref()
    }

    pub(crate) const fn loongarch_platform(&self) -> Option<&super::LoongArchPlatformProfile> {
        self.loongarch_platform.as_ref()
    }
}

/// Immutable policy request consumed by [`super::VmMachinePlanner`].
#[derive(Clone, Debug)]
pub struct VmMachineRequest {
    mode: VmMachineMode,
    firmware: GuestFirmwareKind,
    physical_interrupt_policy: PhysicalInterruptPolicy,
    vcpu_count: usize,
    memory: Vec<GuestMemoryRegion>,
    denied: Vec<HostDeviceSelector>,
    virtual_devices: Vec<VirtualDeviceDescriptor>,
}

impl VmMachineRequest {
    /// Creates a machine request with mediated interrupt delivery.
    pub const fn new(mode: VmMachineMode, firmware: GuestFirmwareKind) -> Self {
        Self {
            mode,
            firmware,
            physical_interrupt_policy: PhysicalInterruptPolicy::Mediated,
            vcpu_count: 1,
            memory: Vec::new(),
            denied: Vec::new(),
            virtual_devices: Vec::new(),
        }
    }

    /// Selects how assigned physical IRQs are forwarded.
    pub fn with_physical_interrupt_policy(mut self, policy: PhysicalInterruptPolicy) -> Self {
        self.physical_interrupt_policy = policy;
        self
    }

    /// Sets the number of vCPU-private controller contexts to plan.
    pub fn with_vcpu_count(mut self, vcpu_count: usize) -> Self {
        self.vcpu_count = vcpu_count;
        self
    }

    /// Adds one explicitly assigned guest memory range.
    pub fn with_memory(mut self, memory: GuestMemoryRegion) -> Self {
        self.memory.push(memory);
        self
    }

    /// Adds a physical-resource deny selector.
    pub fn deny(mut self, selector: HostDeviceSelector) -> Self {
        self.denied.push(selector);
        self
    }

    /// Adds one virtual device instance.
    pub fn with_virtual_device(mut self, device: VirtualDeviceDescriptor) -> Self {
        self.virtual_devices.push(device);
        self
    }

    pub(crate) const fn mode(&self) -> VmMachineMode {
        self.mode
    }

    pub(crate) const fn firmware(&self) -> GuestFirmwareKind {
        self.firmware
    }

    pub(crate) const fn physical_interrupt_policy(&self) -> PhysicalInterruptPolicy {
        self.physical_interrupt_policy
    }

    pub(crate) const fn vcpu_count(&self) -> usize {
        self.vcpu_count
    }

    pub(crate) fn memory(&self) -> &[GuestMemoryRegion] {
        &self.memory
    }

    pub(crate) fn fixed_memory(&self) -> impl Iterator<Item = AddressRange> + '_ {
        self.memory.iter().filter_map(|memory| memory.fixed_range())
    }

    pub(crate) fn denied(&self) -> &[HostDeviceSelector] {
        &self.denied
    }

    pub(crate) fn virtual_devices(&self) -> &[VirtualDeviceDescriptor] {
        &self.virtual_devices
    }
}
