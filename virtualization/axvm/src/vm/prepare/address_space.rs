//! Guest address-space construction for VM preparation.

use std::{ops::Range, vec::Vec};

use axdevice::{DeviceNodeKind, DirectMapping};
use axdevice_base::Resource;
use axvm_types::HostDeviceAssignment;

use super::super::*;

impl AxVMResources {
    pub(crate) fn prepare_guest_address_space(
        &mut self,
        vm_id: usize,
        config: &AxVMConfig,
        architecture_regions: &[GuestOwnedRegion],
        direct_mappings: &[DirectMapping],
    ) -> AxVmResult {
        self.validate_guest_dtb(config)?;
        let mut owned_regions = self.guest_owned_regions(config);
        owned_regions.extend_from_slice(architecture_regions);
        // Direct-mapped BAR ranges must punch holes in passthrough space:
        // the stage-2 update port maps them to the backing HPA directly, so
        // the identity window must not claim those GPAs.
        owned_regions.extend(direct_mappings.iter().map(|mapping| {
            GuestOwnedRegion::new(
                mapping.gpa_base() as usize,
                mapping.size() as usize,
                VmRegionKind::Reserved,
            )
        }));
        self.map_guest_address_space(vm_id, config, &owned_regions, direct_mappings)
    }

    fn validate_guest_dtb(&self, config: &AxVMConfig) -> AxVmResult {
        if config.image_config().dtb_load_gpa.is_some()
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
        config: &AxVMConfig,
        owned_regions: &[GuestOwnedRegion],
        direct_mappings: &[DirectMapping],
    ) -> AxVmResult {
        let graph = self.planned_devices().graph();
        // Direct-mapped BAR ranges leave the emulated trap registration:
        // their guests reach the backing through stage-2 page permissions
        // instead of VM exits.
        let mut direct_ranges = direct_mappings
            .iter()
            .map(|mapping| mapping.gpa_base()..mapping.gpa_base() + mapping.size())
            .collect::<Vec<_>>();
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
            .flat_map(|resource| split_emulated_range(resource, &mut direct_ranges))
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
            config.address_space_policy(),
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
                .lock()
                .map_linear(mapping.gpa, mapping.hpa, mapping.size, mapping.flags)
                .map_err(|error| AxVmError::from_addrspace("map guest address space", error))?;
        }
        // Direct device mappings were installed through the stage-2 update
        // port when their endpoints bound; their GPA ranges were excluded
        // from the emulated registration above, so nothing else claims them.
        for mapping in direct_mappings {
            info!(
                "VM[{vm_id}] stage2 direct mapping: [{:#x}, {:#x}) -> [{:#x}, {:#x}) {} {}",
                mapping.gpa_base(),
                mapping.gpa_base() + mapping.size(),
                mapping.hpa_base(),
                mapping.hpa_base() + mapping.size(),
                mapping.label(),
                if mapping.writable() { "rw" } else { "ro" }
            );
        }
        self.address_layout = Some(address_layout);

        Ok(())
    }

    fn guest_owned_regions(&self, config: &AxVMConfig) -> Vec<GuestOwnedRegion> {
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
        regions.extend(config.reserved_address_ranges().iter().map(|range| {
            GuestOwnedRegion::new(range.base_gpa, range.length, VmRegionKind::Reserved)
        }));

        regions
    }
}

/// Splits one emulated MMIO range around every direct-mapped GPA range.
///
/// A range that no direct mapping touches passes through unchanged; a range
/// fully covered disappears; a partially covered range is cut into the
/// non-overlapping remainders so only trap-backed bytes stay emulated.
fn split_emulated_range(resource: Resource, direct_ranges: &mut [Range<u64>]) -> Vec<Resource> {
    let Resource::MmioRange { base, size } = resource else {
        return vec![resource];
    };
    let start = base;
    let end = base + size;
    let mut cuts = direct_ranges
        .iter()
        .filter_map(|range| {
            let cut_start = range.start.max(start);
            let cut_end = range.end.min(end);
            (cut_start < cut_end).then_some((cut_start, cut_end))
        })
        .collect::<Vec<_>>();
    cuts.sort_unstable();
    let mut pieces = Vec::new();
    let mut cursor = start;
    for (cut_start, cut_end) in cuts {
        if cut_start > cursor {
            pieces.push(Resource::MmioRange {
                base: cursor,
                size: cut_start - cursor,
            });
        }
        cursor = cursor.max(cut_end);
    }
    if cursor < end {
        pieces.push(Resource::MmioRange {
            base: cursor,
            size: end - cursor,
        });
    }
    pieces
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

    #[test]
    fn split_emulated_range_carves_direct_mappings() {
        let resource = Resource::MmioRange {
            base: 0x1000,
            size: 0x4000,
        };
        let mut direct = [0x2000..0x3000, 0x4000..0x5000];
        let pieces = split_emulated_range(resource, &mut direct);
        assert_eq!(
            pieces,
            vec![
                Resource::MmioRange {
                    base: 0x1000,
                    size: 0x1000
                },
                Resource::MmioRange {
                    base: 0x3000,
                    size: 0x1000
                }
            ]
        );
    }

    #[test]
    fn split_emulated_range_drops_fully_covered_ranges() {
        let resource = || Resource::MmioRange {
            base: 0x2000,
            size: 0x1000,
        };
        let mut direct = [0x1000..0x4000, 0x9000..0xa000];
        assert!(split_emulated_range(resource(), &mut direct).is_empty());
        // A range no direct mapping touches passes through unchanged.
        let mut disjoint = [0x8000..0x9000, 0xa000..0xb000];
        assert_eq!(
            split_emulated_range(resource(), &mut disjoint),
            vec![resource()]
        );
    }
}
