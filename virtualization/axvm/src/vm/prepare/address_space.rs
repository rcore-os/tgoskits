//! Guest address-space construction for VM preparation.

use std::{
    collections::{BTreeMap, btree_map::Entry},
    sync::Mutex,
    vec::Vec,
};

use ax_memory_addr::{PAGE_SIZE_4K, align_up_4k};
use axdevice::DeviceNodeKind;
use axdevice_base::Resource;
use axvm_types::{HostDeviceAssignment, HostPhysAddr};

use super::super::*;
use crate::{
    host::{HostMemory, default_host},
    sync::MutexExt,
};

const SHARED_MEMORY_MAPPING_NAME: &str = "shared-memory";

#[derive(Debug, Clone, Copy)]
struct SharedMemoryBacking {
    base_hpa: usize,
    size: usize,
}

static SHARED_MEMORY_BACKINGS: Mutex<BTreeMap<u64, SharedMemoryBacking>> =
    Mutex::new(BTreeMap::new());

impl AxVMResources {
    pub(crate) fn prepare_guest_address_space(
        &mut self,
        vm_id: usize,
        architecture_regions: &[GuestOwnedRegion],
    ) -> AxVmResult {
        self.validate_guest_dtb()?;
        let mut owned_regions = self.guest_owned_regions();
        owned_regions.extend_from_slice(architecture_regions);
        self.map_guest_address_space(vm_id, &owned_regions)
    }

    fn validate_guest_dtb(&self) -> AxVmResult {
        if self.config.image_config().dtb_load_gpa.is_some()
            && self.boot_description.device_tree().is_none()
        {
            return ax_err!(
                InvalidInput,
                "DTB load GPA is configured but no guest device tree bytes are registered"
            );
        }
        Ok(())
    }

    fn map_guest_address_space(
        &mut self,
        vm_id: usize,
        owned_regions: &[GuestOwnedRegion],
    ) -> AxVmResult {
        let graph = self.planned_devices().graph();
        let mut shared_memory_mappings = Vec::new();
        let mut emulated_resources = Vec::new();
        for node in graph.nodes().filter(|node| {
            matches!(
                node.kind(),
                DeviceNodeKind::Virtual | DeviceNodeKind::HostReplacement
            )
        }) {
            let resolved = graph.resources_for(node.id())?;
            for (_slot, shared) in resolved.shared_memory_ranges() {
                let base_gpa = usize::try_from(shared.base()).map_err(|_| {
                    AxVmError::invalid_config("shared-memory GPA does not fit usize")
                })?;
                let size = usize::try_from(shared.size()).map_err(|_| {
                    AxVmError::invalid_config("shared-memory size does not fit usize")
                })?;
                let requested_backing = usize::try_from(shared.host_backing()).map_err(|_| {
                    AxVmError::invalid_config("shared-memory backing does not fit usize")
                })?;
                let base_hpa =
                    shared_memory_backing_for(shared.sharing_key(), size, requested_backing)?;
                shared_memory_mappings.push(HostDeviceAssignment {
                    name: SHARED_MEMORY_MAPPING_NAME.into(),
                    base_gpa,
                    base_hpa,
                    length: size,
                });
            }
            for (slot, base, size) in resolved.mmio_ranges() {
                let _ = slot;
                emulated_resources.push(Resource::MmioRange { base, size });
            }
        }
        let passthrough_devices = graph
            .host_mappings()
            .map(|mapping| {
                Ok(HostDeviceAssignment {
                    name: std::string::String::new(),
                    base_gpa: usize::try_from(mapping.guest_base()).map_err(|_| {
                        AxVmError::invalid_config("planned passthrough GPA does not fit usize")
                    })?,
                    base_hpa: usize::try_from(mapping.host_base()).map_err(|_| {
                        AxVmError::invalid_config("planned passthrough HPA does not fit usize")
                    })?,
                    length: usize::try_from(mapping.length()).map_err(|_| {
                        AxVmError::invalid_config("planned passthrough length does not fit usize")
                    })?,
                })
            })
            .collect::<AxVmResult<Vec<_>>>()?;
        shared_memory_mappings.extend(passthrough_devices);
        let address_layout = build_address_layout(
            self.config.address_space_policy(),
            VM_ASPACE_BASE,
            stage2_guest_address_space_size(self.nested_paging.gpa_bits),
            &shared_memory_mappings,
            &[],
            owned_regions,
            &emulated_resources,
        )?;

        for mapping in address_layout.mappings() {
            debug!(
                "VM[{vm_id}] stage2 {:?}: [{:#x}, {:#x}) -> [{:#x}, {:#x}) {:?}",
                mapping.kind,
                mapping.gpa.as_usize(),
                mapping.gpa.as_usize() + mapping.size,
                mapping.hpa.as_usize(),
                mapping.hpa.as_usize() + mapping.size,
                mapping.flags
            );
            self.address_space
                .map_linear(mapping.gpa, mapping.hpa, mapping.size, mapping.flags)
                .map_err(|error| AxVmError::from_addrspace("map guest address space", error))?;
        }
        self.address_layout = Some(address_layout);

        Ok(())
    }

    fn guest_owned_regions(&self) -> Vec<GuestOwnedRegion> {
        let mut regions = self
            .memory_regions
            .iter()
            .map(|region| {
                GuestOwnedRegion::new(region.gpa.as_usize(), region.size(), VmRegionKind::Memory)
            })
            .collect::<Vec<_>>();

        regions.extend(
            self.boot_description
                .occupied_ranges()
                .map(|(base, length)| {
                    GuestOwnedRegion::new(base, length, VmRegionKind::BootDescription)
                }),
        );
        regions.extend(self.config.reserved_address_ranges().iter().map(|range| {
            GuestOwnedRegion::new(range.base_gpa, range.length, VmRegionKind::Reserved)
        }));

        regions
    }
}

fn shared_memory_backing_for(
    sharing_key: u64,
    size: usize,
    requested_backing: usize,
) -> AxVmResult<usize> {
    let mut backings = SHARED_MEMORY_BACKINGS.lock_unpoisoned();
    match backings.entry(sharing_key) {
        Entry::Occupied(entry) => {
            let backing = entry.get();
            if backing.size != size {
                return Err(AxVmError::invalid_config(format!(
                    "shared-memory key {sharing_key} size mismatch: {size:#x} != {:#x}",
                    backing.size
                )));
            }
            if requested_backing != 0 && requested_backing != backing.base_hpa {
                return Err(AxVmError::invalid_config(format!(
                    "shared-memory key {sharing_key} backing mismatch: {requested_backing:#x} != \
                     {:#x}",
                    backing.base_hpa
                )));
            }
            Ok(backing.base_hpa)
        }
        Entry::Vacant(entry) => {
            let base_hpa = if requested_backing != 0 {
                requested_backing
            } else {
                alloc_shared_memory_backing(size)?
            };
            entry.insert(SharedMemoryBacking { base_hpa, size });
            Ok(base_hpa)
        }
    }
}

fn alloc_shared_memory_backing(size: usize) -> AxVmResult<usize> {
    let size = align_up_4k(size);
    let frames = size / PAGE_SIZE_4K;
    let base_hpa = default_host()
        .alloc_contiguous_frames(frames, PAGE_SIZE_4K)
        .ok_or(AxVmError::OutOfMemory {
            operation: "allocate shared-memory backing",
        })?;
    // The first peer must not observe data left by an earlier host allocation.
    unsafe {
        default_host()
            .phys_to_virt(HostPhysAddr::from(base_hpa.as_usize()))
            .as_mut_ptr()
            .write_bytes(0, size);
    }
    Ok(base_hpa.as_usize())
}

fn stage2_guest_address_space_size(gpa_bits: usize) -> usize {
    if gpa_bits >= usize::BITS as usize {
        VM_ASPACE_SIZE
    } else {
        VM_ASPACE_SIZE.min(1usize << gpa_bits)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guest_address_space_is_capped_by_stage2_gpa_width() {
        assert_eq!(stage2_guest_address_space_size(39), 1usize << 39);
        assert_eq!(stage2_guest_address_space_size(48), VM_ASPACE_SIZE);
    }
}
