//! Normalized host mappings represented as non-runtime graph nodes.

use core::{
    cmp::{max, min},
    ops::Range,
};
use std::{collections::BTreeSet, string::String, vec::Vec};

use axdevice::*;

use crate::{config::*, *};

pub(super) fn add_host_nodes(
    config: &AxVMConfig,
    replacement_ranges: &[Range<u64>],
    builder: &mut DeviceGraphBuilder,
) -> AxVmResult {
    let mut mappings = Vec::new();
    for device in config.pass_through_devices() {
        if device.length == 0 {
            return Err(AxVmError::invalid_config(std::format!(
                "host passthrough device '{}' has no resolved address range",
                device.name
            )));
        }
        let mapping = checked_mapping(device.base_gpa, device.base_hpa, device.length)?;
        let firmware_path = device.name.starts_with('/').then(|| device.name.clone());
        insert_host_mapping(
            &mut mappings,
            HostMappingNode::new(mapping, device.name.clone(), firmware_path),
        )?;
    }

    for (index, address) in config.pass_through_addresses().iter().enumerate() {
        let mapping = checked_mapping(address.base_gpa, address.base_gpa, address.length)?;
        insert_host_mapping(
            &mut mappings,
            HostMappingNode::new(mapping, std::format!("identity-{index}"), None),
        )?;
    }
    subtract_replacement_ranges(&mut mappings, replacement_ranges.to_vec())?;
    mappings.sort_by_key(|node| mapping_key(node.mapping));

    for (index, mapping_node) in mappings.into_iter().enumerate() {
        add_host_mapping_node(builder, index, mapping_node)?;
    }
    Ok(())
}

fn subtract_replacement_ranges(
    mappings: &mut Vec<HostMappingNode>,
    replacement_ranges: Vec<Range<u64>>,
) -> AxVmResult {
    for replacement in replacement_ranges {
        let mut remaining = Vec::new();
        for mapping in core::mem::take(mappings) {
            remaining.extend(mapping.without_range(&replacement)?);
        }
        *mappings = remaining;
    }
    Ok(())
}

fn add_host_mapping_node(
    builder: &mut DeviceGraphBuilder,
    index: usize,
    mapping_node: HostMappingNode,
) -> AxVmResult {
    let mapping = mapping_node.mapping;
    let id = DeviceNodeId::new(std::format!("host-mmio-{index}@{:x}", mapping.guest_base()))?;
    let mut firmware_paths = mapping_node.firmware_paths.into_iter();
    let first_firmware_path = firmware_paths.next();
    let requirements = fixed_mmio(mapping)?;
    let mut node =
        DeviceNodeSpec::host_passthrough(id.clone(), requirements).with_host_mapping(mapping);
    if let Some(path) = first_firmware_path {
        node = node.with_firmware_binding(DeviceFirmwareBinding::FdtNode(path));
    }
    builder
        .add(node)
        .map_err(axdevice::DeviceManagerError::from)?;

    for (binding_index, path) in firmware_paths.enumerate() {
        let firmware_id = DeviceNodeId::new(std::format!(
            "host-firmware-{index}-{binding_index}@{:x}",
            mapping.guest_base()
        ))?;
        builder
            .add(
                DeviceNodeSpec::firmware_only(firmware_id)
                    .with_dependency(id.clone())
                    .with_firmware_binding(DeviceFirmwareBinding::FdtNode(path)),
            )
            .map_err(axdevice::DeviceManagerError::from)?;
    }
    Ok(())
}

/// One canonical linear host mapping and every firmware identity sharing it.
#[derive(Clone)]
struct HostMappingNode {
    mapping: HostPassthroughMapping,
    owners: BTreeSet<String>,
    firmware_paths: BTreeSet<String>,
}

impl HostMappingNode {
    fn new(mapping: HostPassthroughMapping, owner: String, firmware_path: Option<String>) -> Self {
        let mut owners = BTreeSet::new();
        owners.insert(owner);
        let mut firmware_paths = BTreeSet::new();
        firmware_paths.extend(firmware_path);
        Self {
            mapping,
            owners,
            firmware_paths,
        }
    }

    fn merge(&mut self, other: &mut Self) -> AxVmResult<bool> {
        let self_end = mapping_end(self.mapping);
        let other_end = mapping_end(other.mapping);
        let overlaps =
            self.mapping.guest_base() < other_end && other.mapping.guest_base() < self_end;
        let same_linear_mapping = mapping_delta(self.mapping) == mapping_delta(other.mapping);

        if overlaps && !same_linear_mapping {
            return Err(AxVmError::invalid_config(std::format!(
                "host passthrough GPA range {:#x}..{self_end:#x} owned by {:?} conflicts with \
                 {:#x}..{other_end:#x} owned by {:?}: host mappings have different offsets",
                self.mapping.guest_base(),
                self.owners,
                other.mapping.guest_base(),
                other.owners,
            )));
        }
        let adjacent =
            self_end == other.mapping.guest_base() || other_end == self.mapping.guest_base();
        if !same_linear_mapping || (!overlaps && !adjacent) {
            return Ok(false);
        }

        let guest_base = min(self.mapping.guest_base(), other.mapping.guest_base());
        let guest_end = max(self_end, other_end);
        let host_base = if guest_base == self.mapping.guest_base() {
            self.mapping.host_base()
        } else {
            other.mapping.host_base()
        };
        self.mapping = HostPassthroughMapping::new(guest_base, host_base, guest_end - guest_base)?;
        self.owners.append(&mut other.owners);
        self.firmware_paths.append(&mut other.firmware_paths);
        Ok(true)
    }

    fn without_range(self, removed: &Range<u64>) -> AxVmResult<Vec<Self>> {
        let mapping_start = self.mapping.guest_base();
        let mapping_end = mapping_end(self.mapping);
        if mapping_start >= removed.end || removed.start >= mapping_end {
            return Ok(std::vec![self]);
        }

        let mut fragments = Vec::with_capacity(2);
        if mapping_start < removed.start {
            fragments.push(self.fragment(mapping_start, min(mapping_end, removed.start))?);
        }
        if removed.end < mapping_end {
            fragments.push(self.fragment(max(mapping_start, removed.end), mapping_end)?);
        }
        Ok(fragments)
    }

    fn fragment(&self, guest_base: u64, guest_end: u64) -> AxVmResult<Self> {
        let host_offset = guest_base - self.mapping.guest_base();
        let host_base = self
            .mapping
            .host_base()
            .checked_add(host_offset)
            .ok_or_else(|| AxVmError::invalid_config("host mapping fragment overflows HPA"))?;
        Ok(Self {
            mapping: HostPassthroughMapping::new(guest_base, host_base, guest_end - guest_base)?,
            owners: self.owners.clone(),
            firmware_paths: self.firmware_paths.clone(),
        })
    }
}

fn insert_host_mapping(
    mappings: &mut Vec<HostMappingNode>,
    mut candidate: HostMappingNode,
) -> AxVmResult {
    let mut index = 0;
    while index < mappings.len() {
        if candidate.merge(&mut mappings[index])? {
            mappings.remove(index);
            index = 0;
        } else {
            index += 1;
        }
    }
    mappings.push(candidate);
    Ok(())
}

const fn mapping_end(mapping: HostPassthroughMapping) -> u64 {
    mapping.guest_base() + mapping.length()
}

const fn mapping_delta(mapping: HostPassthroughMapping) -> i128 {
    mapping.guest_base() as i128 - mapping.host_base() as i128
}

fn checked_mapping(
    guest_base: usize,
    host_base: usize,
    length: usize,
) -> AxVmResult<HostPassthroughMapping> {
    Ok(HostPassthroughMapping::new(
        u64::try_from(guest_base)
            .map_err(|_| AxVmError::invalid_config("passthrough GPA does not fit u64"))?,
        u64::try_from(host_base)
            .map_err(|_| AxVmError::invalid_config("passthrough HPA does not fit u64"))?,
        u64::try_from(length)
            .map_err(|_| AxVmError::invalid_config("passthrough length does not fit u64"))?,
    )?)
}

fn fixed_mmio(mapping: HostPassthroughMapping) -> AxVmResult<DeviceRequirements> {
    Ok(DeviceRequirements::new().with_mmio(
        ResourceSlot::new("registers")?,
        mapping.length(),
        1,
        ResourceRequest::Fixed(mapping.guest_base()),
    )?)
}

const fn mapping_key(mapping: HostPassthroughMapping) -> (u64, u64, u64) {
    (mapping.guest_base(), mapping.host_base(), mapping.length())
}

#[cfg(test)]
mod tests {
    use axdevice::ResourcePools;
    use axvm_types::HostDeviceAssignment;

    use super::*;
    use crate::config::{AxVMConfigParams, PhysCpuList};

    #[test]
    fn overlapping_linear_host_ranges_share_one_reservation() {
        let mut config = AxVMConfig::default_for_test(1, "overlapping-host-mmio");
        for (name, length) in [("provider", 0x7f00), ("consumer", 0x100)] {
            config.add_pass_through_device(HostDeviceAssignment {
                name: name.into(),
                base_gpa: 0xfdcb_0000,
                base_hpa: 0xfdcb_0000,
                length,
            });
        }

        let mut builder = DeviceGraphBuilder::new();
        add_host_nodes(&config, &[], &mut builder).unwrap();
        let declared = builder.declare().unwrap();
        let requests = declared.requests().unwrap();
        let mut pools = ResourcePools::new();
        super::super::pools::allow_fixed_requirements(&requests, &mut pools).unwrap();
        let graph = declared.resolve(pools).unwrap();

        assert_eq!(
            graph.host_mappings().collect::<std::vec::Vec<_>>(),
            [HostPassthroughMapping::new(0xfdcb_0000, 0xfdcb_0000, 0x7f00).unwrap()]
        );
        assert_eq!(graph.fixed_lease_count(), 1);
    }

    #[test]
    fn host_replacement_range_is_not_reserved_as_passthrough() {
        let config = AxVMConfig::new(AxVMConfigParams {
            phys_cpu_ls: PhysCpuList::new(1, None, None),
            pass_through_devices: std::vec![HostDeviceAssignment {
                name: "clock-controller@fd7c0000".into(),
                base_gpa: 0xfd7c_0000,
                base_hpa: 0xfd7c_0000,
                length: 0x50_000,
            }],
            ..Default::default()
        });

        let mut builder = DeviceGraphBuilder::new();
        add_host_nodes(&config, &[0xfd7c_0000..0xfd81_0000], &mut builder).unwrap();
        let declared = builder.declare().unwrap();
        let requests = declared.requests().unwrap();
        let mut pools = ResourcePools::new();
        super::super::pools::allow_fixed_requirements(&requests, &mut pools).unwrap();
        let graph = declared.resolve(pools).unwrap();

        assert!(graph.host_mappings().next().is_none());
    }
}
