//! Guest address-space construction for VM preparation.

use alloc::vec::Vec;

use ax_errno::AxResult;
use axdevice::AxVmDevices;
use axdevice_base::Resource;
#[cfg(all(target_arch = "x86_64", feature = "vmx"))]
use axvm_types::GuestPhysAddr;
#[cfg(all(target_arch = "x86_64", feature = "vmx"))]
use x86_vcpu::X86_APIC_ACCESS_GPA;

use super::super::{AxVM, AxVMResources, VM_ASPACE_BASE, VM_ASPACE_SIZE};
use crate::layout::{GuestOwnedRegion, VmRegionKind, build_address_layout};

pub(super) fn map_guest_address_space(
    vm: &AxVM,
    resources: &mut AxVMResources,
    devices: &AxVmDevices,
) -> AxResult {
    let owned_regions = guest_owned_regions(resources);
    let emulated_resources = devices
        .devices()
        .flat_map(|device| device.resources().iter().cloned())
        .collect::<Vec<Resource>>();
    let address_layout = build_address_layout(
        resources.config.address_space_policy(),
        VM_ASPACE_BASE,
        VM_ASPACE_SIZE,
        resources.config.pass_through_devices(),
        resources.config.pass_through_addresses(),
        &owned_regions,
        &emulated_resources,
    )?;

    for mapping in address_layout.mappings() {
        debug!(
            "VM[{}] stage2 {:?}: [{:#x}, {:#x}) -> [{:#x}, {:#x}) {:?}",
            vm.id(),
            mapping.kind,
            mapping.gpa.as_usize(),
            mapping.gpa.as_usize() + mapping.size,
            mapping.hpa.as_usize(),
            mapping.hpa.as_usize() + mapping.size,
            mapping.flags
        );
        resources.address_space.map_linear(
            mapping.gpa,
            mapping.hpa,
            mapping.size,
            mapping.flags,
        )?;
    }
    resources.address_layout = Some(address_layout);

    #[cfg(all(target_arch = "x86_64", feature = "vmx"))]
    resources.address_space.map_linear(
        GuestPhysAddr::from(X86_APIC_ACCESS_GPA),
        crate::arch::x86_apic_access_page_addr(),
        ax_memory_addr::PAGE_SIZE_4K,
        axvm_types::MappingFlags::DEVICE
            | axvm_types::MappingFlags::READ
            | axvm_types::MappingFlags::WRITE,
    )?;

    Ok(())
}

fn guest_owned_regions(resources: &AxVMResources) -> Vec<GuestOwnedRegion> {
    let mut regions = resources
        .memory_regions
        .iter()
        .map(|region| {
            GuestOwnedRegion::new(region.gpa.as_usize(), region.size(), VmRegionKind::Memory)
        })
        .collect::<Vec<_>>();

    regions.extend(
        resources
            .boot_description
            .occupied_ranges()
            .map(|(base, length)| {
                GuestOwnedRegion::new(base, length, VmRegionKind::BootDescription)
            }),
    );
    regions.extend(
        resources
            .config
            .reserved_address_ranges()
            .iter()
            .map(|range| {
                GuestOwnedRegion::new(range.base_gpa, range.length, VmRegionKind::Reserved)
            }),
    );

    #[cfg(all(target_arch = "x86_64", feature = "vmx"))]
    regions.push(GuestOwnedRegion::new(
        X86_APIC_ACCESS_GPA,
        ax_memory_addr::PAGE_SIZE_4K,
        VmRegionKind::Reserved,
    ));

    regions
}
