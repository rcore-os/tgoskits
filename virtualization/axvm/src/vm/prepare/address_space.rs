//! Guest address-space construction for VM preparation.

use std::vec::Vec;

use axdevice::DeviceNodeKind;
use axdevice_base::Resource;
use axvm_types::HostDeviceAssignment;

use super::super::*;

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
        let emulated_resources = graph
            .nodes()
            .filter(|node| {
                matches!(
                    node.kind(),
                    DeviceNodeKind::Virtual | DeviceNodeKind::HostReplacement
                )
            })
            .map(|node| graph.resources_for(node.id()))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .flat_map(|resolved| {
                resolved
                    .mmio_ranges()
                    .map(|(_, base, size)| Resource::MmioRange { base, size })
            })
            .collect::<Vec<_>>();
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
        let address_layout = build_address_layout(
            self.config.address_space_policy(),
            VM_ASPACE_BASE,
            stage2_guest_address_space_size(self.nested_paging.gpa_bits),
            &passthrough_devices,
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
