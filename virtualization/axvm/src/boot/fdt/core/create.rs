// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use alloc::{
    collections::BTreeSet,
    string::{String, ToString},
    vec::Vec,
};
use core::ptr::NonNull;

use ax_memory_addr::MemoryAddr;
use axvmconfig::{AxVMCrateConfig, EmulatedDeviceType};
use fdt_edit::{Fdt, Node, NodeId};

use super::tree::{FdtTree, GuestMemorySpec};
use crate::{
    AxVMRef, AxVmResult, GuestPhysAddr, VMMemoryRegion, ax_err_type,
    boot::images::load_vm_image_from_memory,
};

pub fn create_guest_fdt(
    fdt: &Fdt,
    passthrough_device_names: &[String],
    crate_config: &AxVMCrateConfig,
) -> AxVmResult<Vec<u8>> {
    let phys_cpu_ids = crate_config
        .base
        .phys_cpu_ids
        .as_deref()
        .ok_or_else(|| ax_err_type!(InvalidInput, "phys_cpu_ids is missing"))?;

    let interrupt_projection = GuestInterruptCapabilityProjection::from_host(fdt, crate_config);
    let mut guest_tree = FdtTree::clone_filtered(fdt, |node_id, path, node| {
        if interrupt_projection.hides_node(path) {
            return false;
        }
        should_keep_generated_node(
            fdt,
            node_id,
            path,
            node,
            passthrough_device_names,
            phys_cpu_ids,
        )
    })?;
    interrupt_projection.sanitize_references(&mut guest_tree)?;
    Ok(guest_tree.finish())
}

#[derive(Default)]
struct GuestInterruptCapabilityProjection {
    hidden_its_paths: BTreeSet<String>,
    hidden_its_phandles: BTreeSet<u32>,
}

impl GuestInterruptCapabilityProjection {
    fn from_host(fdt: &Fdt, crate_config: &AxVMCrateConfig) -> Self {
        let mut projection = Self::default();
        for node_id in fdt.iter_node_ids() {
            let Some(node) = fdt.node(node_id) else {
                continue;
            };
            if !is_gic_v3_its(node) || configured_its_covers_node(fdt, node_id, crate_config) {
                continue;
            }

            projection.hidden_its_paths.insert(fdt.path_of(node_id));
            for property_name in ["phandle", "linux,phandle"] {
                if let Some(phandle) = node
                    .get_property(property_name)
                    .and_then(|property| property.get_u32())
                {
                    projection.hidden_its_phandles.insert(phandle);
                }
            }
        }
        projection
    }

    fn hides_node(&self, path: &str) -> bool {
        self.hidden_its_paths.contains(path)
    }

    fn sanitize_references(&self, tree: &mut FdtTree) -> AxVmResult {
        if self.hidden_its_paths.is_empty() {
            return Ok(());
        }

        for (node_id, path) in tree.node_paths() {
            tree.edit_node(node_id, |node| {
                if path == "/aliases" {
                    let stale_aliases = node
                        .properties()
                        .iter()
                        .filter_map(|property| {
                            property
                                .as_str()
                                .filter(|target| self.hidden_its_paths.contains(*target))
                                .map(|_| property.name().to_string())
                        })
                        .collect::<Vec<_>>();
                    for property_name in stale_aliases {
                        node.remove_property(&property_name);
                    }
                }

                let remove_msi_parent = node
                    .get_property("msi-parent")
                    .and_then(|property| property.get_u32_iter().next())
                    .is_some_and(|phandle| self.hidden_its_phandles.contains(&phandle));
                if remove_msi_parent {
                    node.remove_property("msi-parent");
                }

                let Some(msi_map) = node.get_property("msi-map").cloned() else {
                    return;
                };
                let cells = msi_map.get_u32_iter().collect::<Vec<_>>();
                let mut retained = Vec::with_capacity(cells.len());
                if cells.len() % 4 == 0 {
                    for entry in cells.chunks_exact(4) {
                        if !self.hidden_its_phandles.contains(&entry[1]) {
                            retained.extend_from_slice(entry);
                        }
                    }
                } else if !cells
                    .iter()
                    .any(|cell| self.hidden_its_phandles.contains(cell))
                {
                    return;
                }

                if retained.is_empty() {
                    node.remove_property("msi-map");
                    node.remove_property("msi-map-mask");
                } else if retained.len() != cells.len() {
                    let mut property = msi_map;
                    property.set_u32_ls(&retained);
                    node.set_property(property);
                }
            })?;
        }
        Ok(())
    }
}

fn is_gic_v3_its(node: &Node) -> bool {
    node.get_property("compatible").is_some_and(|property| {
        property
            .as_str_iter()
            .any(|value| value == "arm,gic-v3-its")
    })
}

fn configured_its_covers_node(fdt: &Fdt, node_id: NodeId, crate_config: &AxVMCrateConfig) -> bool {
    let Some(reg) = fdt
        .view_typed(node_id)
        .and_then(|node| node.regs().into_iter().next())
    else {
        return false;
    };
    let Ok(base_gpa) = usize::try_from(reg.address) else {
        return false;
    };
    let required_length = reg.size.and_then(|size| usize::try_from(size).ok());

    crate_config.devices.emu_devices.iter().any(|device| {
        device.emu_type == EmulatedDeviceType::GPPTITS
            && device.base_gpa == base_gpa
            && required_length.is_none_or(|length| device.length >= length)
    })
}

fn should_keep_generated_node(
    fdt: &Fdt,
    node_id: NodeId,
    node_path: &str,
    node: &Node,
    passthrough_device_names: &[String],
    phys_cpu_ids: &[usize],
) -> bool {
    if node.name().starts_with("memory") {
        return false;
    }

    if node_path == "/cpus" || node_path.starts_with("/cpus/cpu-map") {
        return true;
    }

    if node_path.starts_with("/cpus/cpu@") {
        return need_cpu_node(phys_cpu_ids, fdt, node_id, node_path);
    }

    passthrough_device_names
        .iter()
        .any(|device_path| device_path == node_path)
        || is_descendant_of_passthrough_device(node_path, passthrough_device_names)
        || is_ancestor_of_passthrough_device(node_path, passthrough_device_names)
}

fn is_descendant_of_passthrough_device(
    node_path: &str,
    passthrough_device_names: &[String],
) -> bool {
    passthrough_device_names.iter().any(|passthrough_path| {
        node_path
            .strip_prefix(passthrough_path)
            .is_some_and(|suffix| suffix.starts_with('/'))
    })
}

fn is_ancestor_of_passthrough_device(node_path: &str, passthrough_device_names: &[String]) -> bool {
    passthrough_device_names.iter().any(|passthrough_path| {
        passthrough_path
            .strip_prefix(node_path)
            .is_some_and(|suffix| suffix.starts_with('/'))
            || node_path == "/"
    })
}

fn cpu_node_id(node_path: &str) -> Option<usize> {
    node_path
        .strip_prefix("/cpus/cpu@")
        .and_then(|rest| rest.split('/').next())
        .and_then(|id| usize::from_str_radix(id, 16).ok())
}

fn cpu_reg_address(fdt: &Fdt, node_id: NodeId) -> Option<usize> {
    fdt.view_typed(node_id)
        .and_then(|node| node.regs().first().map(|reg| reg.address as usize))
}

pub(crate) fn need_cpu_node(
    phys_cpu_ids: &[usize],
    fdt: &Fdt,
    node_id: NodeId,
    node_path: &str,
) -> bool {
    if !node_path.starts_with("/cpus/cpu@") {
        return true;
    }

    if let Some(cpu_id) = cpu_node_id(node_path) {
        return phys_cpu_ids.contains(&cpu_id);
    }

    cpu_reg_address(fdt, node_id).is_some_and(|cpu_address| {
        debug!("Checking CPU node {node_path} with address 0x{cpu_address:x}");
        phys_cpu_ids.contains(&cpu_address)
    })
}

fn guest_memory_specs(
    new_memory: &[VMMemoryRegion],
    crate_config: &AxVMCrateConfig,
) -> Vec<GuestMemorySpec> {
    let configured_region_count = if crate_config.kernel.configured_memory_region_count == 0 {
        crate_config.kernel.memory_regions.len()
    } else {
        crate_config
            .kernel
            .configured_memory_region_count
            .min(crate_config.kernel.memory_regions.len())
    };

    if new_memory.len() != crate_config.kernel.memory_regions.len() {
        warn!(
            "VM memory region count {} does not match config region count {}; filtering /memory \
             by zipped order",
            new_memory.len(),
            crate_config.kernel.memory_regions.len()
        );
    }

    new_memory
        .iter()
        .take(configured_region_count)
        .zip(
            crate_config
                .kernel
                .memory_regions
                .iter()
                .take(configured_region_count),
        )
        .map(|(mem, _cfg)| GuestMemorySpec::new(mem.gpa.as_usize() as u64, mem.size() as u64))
        .collect()
}

#[cfg(test)]
fn initrd_range_from_image_config(
    ramdisk: Option<&crate::config::RamdiskInfo>,
) -> Option<(u64, u64)> {
    let ramdisk = ramdisk?;
    let start = ramdisk.load_gpa.as_usize() as u64;
    let size = ramdisk.size? as u64;
    Some((start, start.saturating_add(size)))
}

pub fn update_fdt(
    fdt_src: NonNull<u8>,
    dtb_size: usize,
    vm: AxVMRef,
    crate_config: &AxVMCrateConfig,
) -> AxVmResult {
    let patch_runtime = super::selected_guest_fdt_policy().patch_runtime;
    // SAFETY: `fdt_src` originates from `GuestDtbImage::as_bytes`, and the
    // caller supplies the exact slice length while the image remains borrowed.
    let fdt_bytes = unsafe { core::slice::from_raw_parts(fdt_src.as_ptr(), dtb_size) };
    let new_fdt_bytes = patch_runtime(fdt_bytes, &vm, crate_config)?;

    load_patched_fdt(vm, new_fdt_bytes)
}

fn load_patched_fdt(vm: AxVMRef, new_fdt_bytes: Vec<u8>) -> AxVmResult {
    let dest_addr = calculate_dtb_load_addr(vm.clone(), new_fdt_bytes.len())?;
    debug!(
        "New FDT will be loaded at {:x}, size: 0x{:x}",
        dest_addr,
        new_fdt_bytes.len()
    );
    load_vm_image_from_memory(&new_fdt_bytes, dest_addr, vm.clone())?;
    vm.set_guest_device_tree(dest_addr, new_fdt_bytes)
}

pub fn patch_guest_fdt_for_runtime(
    fdt_bytes: &[u8],
    memory_regions: &[VMMemoryRegion],
    crate_config: &AxVMCrateConfig,
    initrd_start_size: Option<(u64, u64)>,
    create_chosen: bool,
) -> AxVmResult<Vec<u8>> {
    let mut tree = FdtTree::from_bytes(fdt_bytes)?;
    let memory_specs = guest_memory_specs(memory_regions, crate_config);
    tree.rebuild_memory_nodes(&memory_specs)?;
    if create_chosen
        || initrd_start_size.is_some()
        || tree.inner().get_by_path_id("/chosen").is_some()
    {
        tree.patch_chosen(initrd_start_size)?;
    }
    Ok(tree.finish())
}

pub(crate) fn calculate_dtb_load_addr(vm: AxVMRef, fdt_size: usize) -> AxVmResult<GuestPhysAddr> {
    const MB: usize = 1024 * 1024;

    let main_memory =
        vm.memory_regions().first().cloned().ok_or_else(|| {
            ax_err_type!(InvalidInput, "VM has no memory region for DTB placement")
        })?;

    let dtb_addr = vm.with_config(|config| {
        let use_configured_dtb_addr =
            config.image_config.dtb_load_gpa.is_some() && !main_memory.is_identical();

        let dtb_addr = if let Some(configured) = config
            .image_config
            .dtb_load_gpa
            .filter(|_| use_configured_dtb_addr)
        {
            configured
        } else {
            let main_memory_size = main_memory.size().min(512 * MB);
            let addr = (main_memory.gpa + main_memory_size - fdt_size).align_down(2 * MB);
            if fdt_size > main_memory_size {
                error!("DTB size is larger than available memory");
            }
            addr
        };
        config.image_config.dtb_load_gpa = Some(dtb_addr);
        dtb_addr
    });

    Ok(dtb_addr)
}

#[cfg(test)]
mod tests {
    use axvmconfig::AxVMCrateConfig;
    use fdt_edit::{Fdt, Node, Property};
    use fdt_raw::RegInfo;

    use super::{
        super::tree::sanitize_bootargs, cpu_node_id, initrd_range_from_image_config, need_cpu_node,
    };
    use crate::{GuestPhysAddr, config::RamdiskInfo};

    fn prop_u32(name: &str, value: u32) -> Property {
        let mut prop = Property::new(name, alloc::vec![]);
        prop.set_u32_ls(&[value]);
        prop
    }

    fn prop_u32_list(name: &str, values: &[u32]) -> Property {
        let mut prop = Property::new(name, alloc::vec![]);
        prop.set_u32_ls(values);
        prop
    }

    fn prop_string(name: &str, value: &str) -> Property {
        let mut prop = Property::new(name, alloc::vec![]);
        prop.set_string(value);
        prop
    }

    fn host_fdt_with_unbacked_its() -> Fdt {
        let mut fdt = Fdt::new();
        let root = fdt.root_id();
        fdt.node_mut(root)
            .unwrap()
            .set_property(prop_u32("#address-cells", 2));
        fdt.node_mut(root)
            .unwrap()
            .set_property(prop_u32("#size-cells", 2));

        let aliases = fdt.add_node(root, Node::new("aliases"));
        fdt.node_mut(aliases).unwrap().set_property(prop_string(
            "its0",
            "/interrupt-controller@fe600000/msi-controller@fe640000",
        ));

        let gic = fdt.add_node(root, Node::new("interrupt-controller@fe600000"));
        fdt.node_mut(gic)
            .unwrap()
            .set_property(prop_u32("#address-cells", 2));
        fdt.node_mut(gic)
            .unwrap()
            .set_property(prop_u32("#size-cells", 2));

        let its = fdt.add_node(gic, Node::new("msi-controller@fe640000"));
        fdt.node_mut(its)
            .unwrap()
            .set_property(prop_string("compatible", "arm,gic-v3-its"));
        fdt.node_mut(its)
            .unwrap()
            .set_property(prop_u32("phandle", 0x10e));
        fdt.view_typed_mut(its)
            .unwrap()
            .set_regs(&[RegInfo::new(0xfe64_0000, Some(0x2_0000))]);

        let pcie = fdt.add_node(root, Node::new("pcie@fe180000"));
        fdt.node_mut(pcie)
            .unwrap()
            .set_property(prop_u32_list("msi-map", &[0x3000, 0x10e, 0x3000, 0x1000]));
        fdt.node_mut(pcie)
            .unwrap()
            .set_property(prop_u32("msi-map-mask", 0xffff));
        fdt.node_mut(pcie)
            .unwrap()
            .set_property(prop_u32_list("interrupts", &[0, 0xf8, 4]));
        fdt
    }

    fn test_fdt(dts: &str) -> Fdt {
        let mut fdt = Fdt::new();
        let root = fdt.root_id();
        let cpus = fdt.add_node(root, Node::new("cpus"));
        fdt.node_mut(cpus)
            .unwrap()
            .set_property(prop_u32("#address-cells", 2));
        fdt.node_mut(cpus)
            .unwrap()
            .set_property(prop_u32("#size-cells", 0));

        for line in dts.lines().map(str::trim).filter(|line| !line.is_empty()) {
            let (name, reg) = line.split_once('=').unwrap();
            let node = fdt.add_node(cpus, Node::new(name));
            let reg = usize::from_str_radix(reg, 16).unwrap();
            fdt.view_typed_mut(node)
                .unwrap()
                .set_regs(&[RegInfo::new(reg as u64, None)]);
        }

        fdt
    }

    #[test]
    fn cpu_node_selection_uses_node_id_when_reg_differs() {
        let fdt = test_fdt("cpu@0=200\ncpu@100=0\ncpu@101=100");
        let selected: alloc::vec::Vec<_> = fdt
            .iter_node_ids()
            .map(|id| (id, fdt.path_of(id)))
            .filter(|(_, path)| path.starts_with("/cpus/cpu@"))
            .filter_map(|(id, path)| need_cpu_node(&[0x100], &fdt, id, &path).then_some(path))
            .collect();

        assert_eq!(selected, ["/cpus/cpu@100"]);
    }

    #[test]
    fn cpu_node_id_parses_hex_unit_address() {
        assert_eq!(cpu_node_id("/cpus/cpu@100"), Some(0x100));
    }

    #[test]
    fn initrd_range_requires_both_address_and_size() {
        assert_eq!(
            initrd_range_from_image_config(Some(&RamdiskInfo {
                load_gpa: GuestPhysAddr::from(0xa000_0000usize),
                size: None,
            })),
            None
        );
        assert_eq!(
            initrd_range_from_image_config(Some(&RamdiskInfo {
                load_gpa: GuestPhysAddr::from(0xa000_0000usize),
                size: Some(0x1234),
            })),
            Some((0xa000_0000, 0xa000_1234))
        );
    }

    #[test]
    fn sanitize_bootargs_enables_auto_repair_for_block_roots() {
        let bootargs = "root=/dev/mmcblk0p2 rw console=ttyS2,1500000 rootwait rootfstype=ext4";

        assert_eq!(
            sanitize_bootargs(bootargs),
            "root=/dev/mmcblk0p2 rw console=ttyS2,1500000 rootwait rootfstype=ext4 fsck.repair=yes"
        );
    }

    #[test]
    fn sanitize_bootargs_preserves_existing_fsck_policy() {
        let bootargs =
            "root=/dev/mmcblk0p2 ro rootwait rootfstype=ext4 fsckfix rdinit=/init root=/dev/ram0";

        assert_eq!(
            sanitize_bootargs(bootargs),
            "root=/dev/mmcblk0p2 rw rootwait rootfstype=ext4 fsckfix"
        );
    }

    #[test]
    fn runtime_patch_can_leave_missing_chosen_for_host_copy() {
        let fdt = Fdt::new();
        let dtb = fdt.encode().as_ref().to_vec();
        let cfg = AxVMCrateConfig::default();

        let patched = super::patch_guest_fdt_for_runtime(&dtb, &[], &cfg, None, false).unwrap();
        let reparsed = Fdt::from_bytes(&patched).unwrap();

        assert!(reparsed.get_by_path_id("/chosen").is_none());

        let patched = super::patch_guest_fdt_for_runtime(&dtb, &[], &cfg, None, true).unwrap();
        let reparsed = Fdt::from_bytes(&patched).unwrap();

        assert!(reparsed.get_by_path_id("/chosen").is_some());
    }

    #[test]
    fn generated_fdt_filters_cpu_nodes_by_unit_address() {
        let fdt = test_fdt("cpu@0=200\ncpu@100=0\ncpu@101=100");
        let cfg = AxVMCrateConfig {
            base: axvmconfig::VMBaseConfig {
                phys_cpu_ids: Some(alloc::vec![0x100]),
                ..Default::default()
            },
            ..Default::default()
        };
        let dtb = super::create_guest_fdt(&fdt, &[], &cfg).unwrap();
        let reparsed = Fdt::from_bytes(&dtb).unwrap();

        assert!(reparsed.get_by_path_id("/cpus/cpu@100").is_some());
        assert!(reparsed.get_by_path_id("/cpus/cpu@0").is_none());
        assert!(reparsed.get_by_path_id("/cpus/cpu@101").is_none());
    }

    #[test]
    fn generated_fdt_hides_unbacked_its_capability() {
        let fdt = host_fdt_with_unbacked_its();
        let cfg = AxVMCrateConfig {
            base: axvmconfig::VMBaseConfig {
                phys_cpu_ids: Some(alloc::vec![0]),
                ..Default::default()
            },
            ..Default::default()
        };
        let passthrough = [
            "/aliases".into(),
            "/interrupt-controller@fe600000".into(),
            "/pcie@fe180000".into(),
        ];

        let dtb = super::create_guest_fdt(&fdt, &passthrough, &cfg).unwrap();
        let reparsed = Fdt::from_bytes(&dtb).unwrap();

        assert!(
            reparsed
                .get_by_path_id("/interrupt-controller@fe600000/msi-controller@fe640000")
                .is_none()
        );
        let pcie = reparsed.get_by_path("/pcie@fe180000").unwrap();
        assert!(pcie.as_node().get_property("msi-map").is_none());
        assert!(pcie.as_node().get_property("msi-map-mask").is_none());
        assert!(pcie.as_node().get_property("interrupts").is_some());
        let aliases = reparsed.get_by_path("/aliases").unwrap();
        assert!(aliases.as_node().get_property("its0").is_none());
    }

    #[test]
    fn generated_fdt_keeps_configured_its_capability() {
        let fdt = host_fdt_with_unbacked_its();
        let cfg = AxVMCrateConfig {
            base: axvmconfig::VMBaseConfig {
                phys_cpu_ids: Some(alloc::vec![0]),
                ..Default::default()
            },
            devices: axvmconfig::VMDevicesConfig {
                emu_devices: alloc::vec![axvmconfig::EmulatedDeviceConfig {
                    name: "gppt-gits".into(),
                    base_gpa: 0xfe64_0000,
                    length: 0x2_0000,
                    irq_id: 0,
                    emu_type: axvmconfig::EmulatedDeviceType::GPPTITS,
                    cfg_list: alloc::vec![0xfe64_0000],
                }],
                ..Default::default()
            },
            ..Default::default()
        };
        let passthrough = [
            "/aliases".into(),
            "/interrupt-controller@fe600000".into(),
            "/pcie@fe180000".into(),
        ];

        let dtb = super::create_guest_fdt(&fdt, &passthrough, &cfg).unwrap();
        let reparsed = Fdt::from_bytes(&dtb).unwrap();

        assert!(
            reparsed
                .get_by_path_id("/interrupt-controller@fe600000/msi-controller@fe640000")
                .is_some()
        );
        let pcie = reparsed.get_by_path("/pcie@fe180000").unwrap();
        assert!(pcie.as_node().get_property("msi-map").is_some());
        assert!(pcie.as_node().get_property("msi-map-mask").is_some());
        let aliases = reparsed.get_by_path("/aliases").unwrap();
        assert!(aliases.as_node().get_property("its0").is_some());
    }
}
