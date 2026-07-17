//! Named virtual-device resource allocation and host-template resolution.

use alloc::{collections::BTreeSet, string::ToString, vec::Vec};

use axdevice::{
    ControllerInputId, DeviceRequirement, MsiDeviceId, MsiEventId, ResolvedDeviceResources,
};
use axvm_types::VmMachineMode;
use vm_allocator::{AddressAllocator, AllocPolicy};

use super::{
    ResolvedInterrupt, ResolvedMmio, ResolvedPio, ResolvedVirtualDevice, ResolvedVirtualDeviceParts,
};
use crate::machine::{
    AddressRange, DeviceInstanceId, HostDeviceDescriptor, HostPlatformSnapshot, IoPortRange,
    MachinePlanError, MachinePlanResult, MachineProfile, VirtualDeviceDescriptor, VmMachineRequest,
};

pub(super) struct ResourceAllocators {
    mmio: AddressAllocator,
    pio: Option<AddressAllocator>,
    interrupts: AddressAllocator,
}

impl ResourceAllocators {
    pub(super) fn new(
        profile: &MachineProfile,
        request: &VmMachineRequest,
        snapshot: &HostPlatformSnapshot,
    ) -> MachinePlanResult<Self> {
        let mmio_pool = profile.mmio_pool();
        let mut mmio =
            AddressAllocator::new(mmio_pool.base(), mmio_pool.size()).map_err(|source| {
                MachinePlanError::ResourceAllocation {
                    resource: "MMIO pool",
                    owner: "machine profile".into(),
                    source,
                }
            })?;
        let interrupt_start = u64::from(*profile.interrupt_pool().start());
        let interrupt_size = u64::from(*profile.interrupt_pool().end()) - interrupt_start + 1;
        let mut interrupts =
            AddressAllocator::new(interrupt_start, interrupt_size).map_err(|source| {
                MachinePlanError::ResourceAllocation {
                    resource: "interrupt pool",
                    owner: "machine profile".into(),
                    source,
                }
            })?;

        reserve_occupied_mmio(profile, request, snapshot, mmio_pool, &mut mmio)?;
        reserve_occupied_interrupts(profile, request, snapshot, &mut interrupts)?;

        let mut pio = profile
            .pio_pool()
            .map(|pool| AddressAllocator::new(u64::from(pool.base()), u64::from(pool.size())))
            .transpose()
            .map_err(|source| MachinePlanError::ResourceAllocation {
                resource: "PIO pool",
                owner: "machine profile".into(),
                source,
            })?;
        if let (Some(pool), Some(allocator)) = (profile.pio_pool(), pio.as_mut()) {
            reserve_occupied_pio(request, snapshot, pool, allocator)?;
        }

        Ok(Self {
            mmio,
            pio,
            interrupts,
        })
    }

    fn allocate_mmio(
        &mut self,
        owner: &DeviceInstanceId,
        size: u64,
        alignment: u64,
    ) -> MachinePlanResult<AddressRange> {
        let range = self
            .mmio
            .allocate(size, alignment, AllocPolicy::FirstMatch)
            .map_err(|source| MachinePlanError::ResourceAllocation {
                resource: "MMIO",
                owner: owner.to_string(),
                source,
            })?;
        AddressRange::new(range.start(), range.len())
    }

    fn allocate_interrupt(&mut self, owner: &DeviceInstanceId) -> MachinePlanResult<u32> {
        let range = self
            .interrupts
            .allocate(1, 1, AllocPolicy::FirstMatch)
            .map_err(|source| MachinePlanError::ResourceAllocation {
                resource: "interrupt",
                owner: owner.to_string(),
                source,
            })?;
        Ok(range.start() as u32)
    }

    fn allocate_pio(
        &mut self,
        owner: &DeviceInstanceId,
        size: u16,
        alignment: u16,
    ) -> MachinePlanResult<IoPortRange> {
        let allocator = self
            .pio
            .as_mut()
            .ok_or_else(|| MachinePlanError::MissingResourcePool {
                resource: "PIO",
                device: owner.to_string(),
            })?;
        let range = allocator
            .allocate(
                u64::from(size),
                u64::from(alignment),
                AllocPolicy::FirstMatch,
            )
            .map_err(|source| MachinePlanError::ResourceAllocation {
                resource: "PIO",
                owner: owner.to_string(),
                source,
            })?;
        IoPortRange::new(range.start() as u16, range.len() as u16)
    }
}

pub(super) fn resolve_virtual_device(
    device: &VirtualDeviceDescriptor,
    template: Option<&HostDeviceDescriptor>,
    allocators: &mut ResourceAllocators,
    msi_device: MsiDeviceId,
) -> MachinePlanResult<ResolvedVirtualDevice> {
    let mut mmio_index = 0;
    let mut pio_index = 0;
    let mut interrupt_index = 0;
    let mut msi_event = 0;
    let mut mmio = Vec::new();
    let mut pio = Vec::new();
    let mut interrupts = Vec::new();
    let mut resources = ResolvedDeviceResources::new();

    for requirement in device.requirements().entries() {
        match requirement {
            DeviceRequirement::Mmio {
                slot,
                size,
                alignment,
            } => {
                let range = match template {
                    Some(template) => {
                        let index = mmio_index;
                        mmio_index += 1;
                        resolve_template_mmio(template, index, *size, *alignment)?
                    }
                    None => allocators.allocate_mmio(device.instance_id(), *size, *alignment)?,
                };
                resources = resources
                    .with_mmio(slot.clone(), range.base(), range.size())
                    .map_err(|source| MachinePlanError::DeviceResource {
                        device: device.instance_id().to_string(),
                        source,
                    })?;
                mmio.push(ResolvedMmio::new(slot.clone(), range));
            }
            DeviceRequirement::Pio {
                slot,
                size,
                alignment,
            } => {
                let range = match template {
                    Some(template) => {
                        let index = pio_index;
                        pio_index += 1;
                        resolve_template_pio(template, index, *size, *alignment)?
                    }
                    None => allocators.allocate_pio(device.instance_id(), *size, *alignment)?,
                };
                resources = resources
                    .with_pio(slot.clone(), range.base(), range.size())
                    .map_err(|source| MachinePlanError::DeviceResource {
                        device: device.instance_id().to_string(),
                        source,
                    })?;
                pio.push(ResolvedPio::new(slot.clone(), range));
            }
            DeviceRequirement::WiredIrq { slot, trigger, .. } => {
                let id = match template {
                    Some(template) => {
                        let index = interrupt_index;
                        interrupt_index += 1;
                        resolve_template_interrupt(template, index, *trigger)?
                    }
                    None => allocators.allocate_interrupt(device.instance_id())?,
                };
                resources = resources
                    .with_wired_irq(slot.clone(), ControllerInputId::new(id as usize), *trigger)
                    .map_err(|source| MachinePlanError::DeviceResource {
                        device: device.instance_id().to_string(),
                        source,
                    })?;
                interrupts.push(ResolvedInterrupt::new(slot.clone(), id, *trigger));
            }
            DeviceRequirement::Msi { slot, .. } => {
                resources = resources
                    .with_msi(slot.clone(), msi_device, MsiEventId::new(msi_event))
                    .map_err(|source| MachinePlanError::DeviceResource {
                        device: device.instance_id().to_string(),
                        source,
                    })?;
                msi_event += 1;
            }
        }
    }

    Ok(ResolvedVirtualDevice::from_parts(
        ResolvedVirtualDeviceParts {
            instance_id: device.instance_id().clone(),
            model_id: device.model_id().clone(),
            host_template: template.map(|template| template.id().clone()),
            mmio,
            pio,
            interrupts,
            resources,
            backend: device.backend(),
        },
    ))
}

fn reserve_occupied_mmio(
    profile: &MachineProfile,
    request: &VmMachineRequest,
    snapshot: &HostPlatformSnapshot,
    pool: AddressRange,
    allocator: &mut AddressAllocator,
) -> MachinePlanResult<()> {
    let mut reservations = profile.reserved_mmio().to_vec();
    reservations.extend(request.fixed_memory());
    if request.mode() == VmMachineMode::Passthrough {
        reservations.extend(
            snapshot
                .devices()
                .iter()
                .flat_map(|device| device.mmio().iter().copied()),
        );
    }
    for reservation in super::mapping::merge_ranges(reservations) {
        reserve_mmio(allocator, pool, reservation)?;
    }
    Ok(())
}

fn reserve_occupied_interrupts(
    profile: &MachineProfile,
    request: &VmMachineRequest,
    snapshot: &HostPlatformSnapshot,
    allocator: &mut AddressAllocator,
) -> MachinePlanResult<()> {
    let mut reserved = BTreeSet::new();
    reserved.extend(profile.reserved_interrupts().iter().copied());
    if request.mode() == VmMachineMode::Passthrough {
        reserved.extend(
            snapshot
                .devices()
                .iter()
                .flat_map(|device| device.interrupts())
                .map(crate::machine::HostInterruptResource::input_u32),
        );
    }
    for interrupt in reserved {
        if profile.interrupt_pool().contains(&interrupt) {
            reserve_interrupt(allocator, interrupt)?;
        }
    }
    Ok(())
}

fn reserve_occupied_pio(
    request: &VmMachineRequest,
    snapshot: &HostPlatformSnapshot,
    pool: IoPortRange,
    allocator: &mut AddressAllocator,
) -> MachinePlanResult<()> {
    if request.mode() != VmMachineMode::Passthrough {
        return Ok(());
    }
    let pool = AddressRange::new(u64::from(pool.base()), u64::from(pool.size()))?;
    let reservations = snapshot
        .devices()
        .iter()
        .flat_map(|device| device.pio())
        .map(|range| AddressRange::new(u64::from(range.base()), u64::from(range.size())))
        .collect::<MachinePlanResult<Vec<_>>>()?;
    for reservation in super::mapping::merge_ranges(reservations) {
        reserve_pool_range(
            allocator,
            pool,
            reservation,
            "reserved PIO",
            "host platform",
        )?;
    }
    Ok(())
}

fn resolve_template_pio(
    template: &HostDeviceDescriptor,
    index: usize,
    size: u16,
    alignment: u16,
) -> MachinePlanResult<IoPortRange> {
    let Some(resource) = template.pio().get(index).copied() else {
        return Err(template_resource_mismatch(template, "PIO", index));
    };
    if resource.size() < size || resource.base() % alignment != 0 {
        return Err(template_resource_mismatch(template, "PIO", index));
    }
    IoPortRange::new(resource.base(), size)
}

fn resolve_template_mmio(
    template: &HostDeviceDescriptor,
    index: usize,
    size: u64,
    alignment: u64,
) -> MachinePlanResult<AddressRange> {
    let Some(resource) = template.mmio().get(index).copied() else {
        return Err(template_resource_mismatch(template, "MMIO", index));
    };
    if resource.size() < size || resource.base() % alignment != 0 {
        return Err(template_resource_mismatch(template, "MMIO", index));
    }
    AddressRange::new(resource.base(), size)
}

fn resolve_template_interrupt(
    template: &HostDeviceDescriptor,
    index: usize,
    required_trigger: axvm_types::InterruptTriggerMode,
) -> MachinePlanResult<u32> {
    let interrupt = template
        .interrupts()
        .get(index)
        .ok_or_else(|| template_resource_mismatch(template, "interrupt", index))?;
    if interrupt.trigger() != required_trigger {
        return Err(MachinePlanError::HostTemplateInterruptTriggerMismatch {
            device: template.id().to_string(),
            index,
            expected: required_trigger,
            actual: interrupt.trigger(),
        });
    }
    Ok(interrupt.input_u32())
}

fn template_resource_mismatch(
    template: &HostDeviceDescriptor,
    resource: &'static str,
    index: usize,
) -> MachinePlanError {
    MachinePlanError::HostTemplateResourceMismatch {
        device: template.id().to_string(),
        resource,
        index,
    }
}

fn reserve_mmio(
    allocator: &mut AddressAllocator,
    pool: AddressRange,
    reservation: AddressRange,
) -> MachinePlanResult<()> {
    reserve_pool_range(
        allocator,
        pool,
        reservation,
        "reserved MMIO",
        "machine profile",
    )
}

fn reserve_pool_range(
    allocator: &mut AddressAllocator,
    pool: AddressRange,
    reservation: AddressRange,
    resource: &'static str,
    owner: &'static str,
) -> MachinePlanResult<()> {
    let Some(reservation) = pool.intersection(reservation) else {
        return Ok(());
    };
    // vm-allocator's ExactMatch lookup probes `[start, start + 1]`, so the final byte of a
    // pool cannot be found that way. LastMatch is exact for this one-byte boundary case; the
    // returned range is still checked below before it is accepted.
    let policy = if reservation.base() == allocator.end() && reservation.size() == 1 {
        AllocPolicy::LastMatch
    } else {
        AllocPolicy::ExactMatch(reservation.base())
    };
    let allocated = allocator
        .allocate(reservation.size(), 1, policy)
        .map_err(|source| MachinePlanError::ResourceAllocation {
            resource,
            owner: owner.into(),
            source,
        })?;
    if allocated.start() != reservation.base() || allocated.len() != reservation.size() {
        return Err(MachinePlanError::ResourceAllocation {
            resource,
            owner: owner.into(),
            source: vm_allocator::Error::ResourceNotAvailable,
        });
    }
    Ok(())
}

fn reserve_interrupt(allocator: &mut AddressAllocator, interrupt: u32) -> MachinePlanResult<()> {
    let address = u64::from(interrupt);
    // vm-allocator 0.1.4 cannot ExactMatch the final inclusive address because
    // its candidate lookup probes `start + 1`. LastMatch is equivalent while
    // that final slot is free; verify the result so an occupied slot cannot
    // silently reserve a different interrupt.
    let policy = if address == allocator.end() {
        AllocPolicy::LastMatch
    } else {
        AllocPolicy::ExactMatch(address)
    };
    let reservation = allocator.allocate(1, 1, policy).map_err(|source| {
        MachinePlanError::ResourceAllocation {
            resource: "reserved interrupt",
            owner: interrupt.to_string(),
            source,
        }
    })?;
    if reservation.start() != address {
        return Err(MachinePlanError::ResourceAllocation {
            resource: "reserved interrupt",
            owner: interrupt.to_string(),
            source: vm_allocator::Error::ResourceNotAvailable,
        });
    }
    Ok(())
}
