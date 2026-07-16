//! Physical-device disposition and passthrough identity-map holes.

use alloc::{collections::BTreeSet, vec::Vec};

use axvm_types::VmMachineMode;

use super::PlannedHostDevice;
use crate::machine::{
    AddressRange, DeviceDisposition, HostDeviceOwnership, HostDeviceSelector, HostPlatformSnapshot,
    MachineProfile, VmMachineRequest,
};

pub(super) fn plan_host_devices(
    mode: VmMachineMode,
    snapshot: &HostPlatformSnapshot,
    denied: &BTreeSet<usize>,
    virtual_templates: &BTreeSet<usize>,
) -> Vec<PlannedHostDevice> {
    if mode == VmMachineMode::Virtual {
        return Vec::new();
    }
    snapshot
        .devices()
        .iter()
        .enumerate()
        .map(|(index, device)| {
            PlannedHostDevice::new(
                device.clone(),
                match device.ownership() {
                    HostDeviceOwnership::HostExclusive => DeviceDisposition::HostExclusive,
                    HostDeviceOwnership::Unrepresentable => DeviceDisposition::Unrepresentable,
                    _ if denied.contains(&index) => DeviceDisposition::Denied,
                    _ if virtual_templates.contains(&index) => {
                        DeviceDisposition::VirtualReplacement
                    }
                    HostDeviceOwnership::Structural => DeviceDisposition::Structural,
                    HostDeviceOwnership::Assignable | HostDeviceOwnership::Transferable => {
                        DeviceDisposition::Passthrough
                    }
                },
            )
        })
        .collect()
}

pub(super) fn plan_identity_mappings(
    profile: &MachineProfile,
    request: &VmMachineRequest,
    snapshot: &HostPlatformSnapshot,
    denied: &BTreeSet<usize>,
    virtual_holes: &[AddressRange],
) -> Vec<AddressRange> {
    if request.mode() == VmMachineMode::Virtual {
        return Vec::new();
    }

    let mut holes = request
        .memory()
        .iter()
        .map(|memory| memory.range())
        .collect::<Vec<_>>();
    holes.extend_from_slice(profile.reserved_mmio());
    for (index, device) in snapshot.devices().iter().enumerate() {
        if denied.contains(&index)
            || matches!(
                device.ownership(),
                HostDeviceOwnership::HostExclusive | HostDeviceOwnership::Unrepresentable
            )
        {
            holes.extend_from_slice(device.mmio());
        }
    }
    for selector in request.denied() {
        if let HostDeviceSelector::Mmio(range) = selector {
            holes.push(*range);
        }
    }
    holes.extend_from_slice(virtual_holes);
    let holes = merge_ranges(holes);

    snapshot
        .io_apertures()
        .iter()
        .flat_map(|aperture| subtract_holes(*aperture, &holes))
        .collect()
}

pub(super) fn merge_ranges(mut ranges: Vec<AddressRange>) -> Vec<AddressRange> {
    ranges.sort_by_key(|range| (range.base(), range.end()));
    let mut merged: Vec<AddressRange> = Vec::with_capacity(ranges.len());
    for range in ranges {
        if let Some(last) = merged.last_mut()
            && range.base() <= last.end()
        {
            let end = last.end().max(range.end());
            if let Some(combined) = AddressRange::from_bounds(last.base(), end) {
                *last = combined;
            }
            continue;
        }
        merged.push(range);
    }
    merged
}

fn subtract_holes(aperture: AddressRange, holes: &[AddressRange]) -> Vec<AddressRange> {
    let mut cursor = aperture.base();
    let mut mappings = Vec::new();
    for hole in holes {
        let Some(hole) = aperture.intersection(*hole) else {
            continue;
        };
        if cursor < hole.base()
            && let Some(mapping) = AddressRange::from_bounds(cursor, hole.base())
        {
            mappings.push(mapping);
        }
        cursor = cursor.max(hole.end());
        if cursor >= aperture.end() {
            break;
        }
    }
    if let Some(mapping) = AddressRange::from_bounds(cursor, aperture.end()) {
        mappings.push(mapping);
    }
    mappings
}
