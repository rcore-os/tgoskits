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

use std::{ptr::NonNull, string::String, vec::Vec};

use ax_memory_addr::MemoryAddr;
use axdevice_base::InterruptTrigger;
use axvmconfig::GuestConfig;
use fdt_edit::{Fdt, Node, NodeId, Property};
use fdt_raw::RegInfo;

use super::tree::{FdtTree, GuestMemorySpec, prop_string};
pub(crate) use crate::boot::fdt::device::{
    ResolvedFdtDevice, ResolvedFdtInterrupt, ResolvedFdtProperty,
};
use crate::{
    AxVMRef, AxVmResult, GuestPhysAddr, VMMemoryRegion, ax_err_type,
    boot::images::load_vm_image_from_memory,
};

pub(crate) fn create_guest_fdt(
    fdt: &Fdt,
    passthrough_device_names: &[String],
    crate_config: &GuestConfig,
    excluded_device_paths: &[String],
) -> AxVmResult<Vec<u8>> {
    let phys_cpu_ids = crate_config
        .base
        .phys_cpu_ids
        .as_deref()
        .ok_or_else(|| ax_err_type!(InvalidInput, "phys_cpu_ids is missing"))?;
    let machine_interrupt_providers = fdt
        .iter_node_ids()
        .filter_map(|node_id| {
            let node = fdt.node(node_id)?;
            is_machine_interrupt_provider(node).then(|| fdt.path_of(node_id))
        })
        .collect::<Vec<_>>();

    let policy = GeneratedNodePolicy {
        fdt,
        passthrough_device_names,
        phys_cpu_ids,
        machine_interrupt_providers: &machine_interrupt_providers,
        excluded_device_paths,
    };
    let mut guest_tree = FdtTree::clone_filtered(fdt, |node_id, path, node| {
        policy.should_keep(node_id, path, node)
    })?;
    prune_dangling_interrupts_extended(fdt, &mut guest_tree)?;
    Ok(guest_tree.finish())
}

struct GeneratedNodePolicy<'a> {
    fdt: &'a Fdt,
    passthrough_device_names: &'a [String],
    phys_cpu_ids: &'a [usize],
    machine_interrupt_providers: &'a [String],
    excluded_device_paths: &'a [String],
}

impl GeneratedNodePolicy<'_> {
    fn should_keep(&self, node_id: NodeId, node_path: &str, node: &Node) -> bool {
        if node.name().starts_with("memory") {
            return false;
        }

        if node_path == "/cpus" || node_path.starts_with("/cpus/cpu-map") {
            return true;
        }

        if node_path.starts_with("/cpus/cpu@") {
            return need_cpu_node(self.phys_cpu_ids, self.fdt, node_id, node_path);
        }

        if self
            .machine_interrupt_providers
            .iter()
            .any(|controller| is_path_or_ancestor(node_path, controller))
        {
            return true;
        }

        if node
            .compatibles()
            .any(|compatible| matches!(compatible, "arm,psci" | "arm,psci-0.2" | "arm,psci-1.0"))
        {
            return true;
        }

        if self.excluded_device_paths.iter().any(|path| {
            node_path == path
                || node_path
                    .strip_prefix(path)
                    .is_some_and(|suffix| suffix.starts_with('/'))
        }) {
            return false;
        }

        self.passthrough_device_names
            .iter()
            .any(|device_path| device_path == node_path)
            || is_descendant_of_passthrough_device(node_path, self.passthrough_device_names)
            || is_ancestor_of_passthrough_device(node_path, self.passthrough_device_names)
    }
}

fn is_machine_interrupt_provider(node: &Node) -> bool {
    node.compatibles().any(|compatible| {
        compatible == "arm,gic-v3-its"
            || (node.get_property("interrupt-controller").is_some()
                && matches!(
                    compatible,
                    "arm,gic-v3"
                        | "arm,cortex-a15-gic"
                        | "arm,gic-400"
                        | "riscv,plic0"
                        | "sifive,plic-1.0.0"
                ))
    })
}

fn is_path_or_ancestor(candidate: &str, path: &str) -> bool {
    candidate == path
        || path
            .strip_prefix(candidate)
            .is_some_and(|suffix| candidate == "/" || suffix.starts_with('/'))
}

fn prune_dangling_interrupts_extended(source: &Fdt, guest: &mut FdtTree) -> AxVmResult {
    let nodes = guest
        .inner()
        .iter_node_ids()
        .filter_map(|node_id| {
            guest
                .inner()
                .node(node_id)?
                .get_property("interrupts-extended")
                .map(|_| (node_id, guest.inner().path_of(node_id)))
        })
        .collect::<Vec<_>>();

    for (node_id, path) in nodes {
        let property = source
            .get_by_path(&path)
            .and_then(|node| node.as_node().get_property("interrupts-extended"))
            .ok_or_else(|| {
                ax_err_type!(
                    InvalidData,
                    std::format!("source FDT node {path} lost interrupts-extended")
                )
            })?;
        let cells = property.get_u32_iter().collect::<Vec<_>>();
        let mut filtered = Vec::with_capacity(cells.len());
        let mut cursor = 0;
        while cursor < cells.len() {
            let phandle = cells[cursor];
            let provider = find_node_by_phandle(source, phandle)
                .and_then(|node_id| source.node(node_id))
                .ok_or_else(|| {
                    ax_err_type!(
                        InvalidData,
                        std::format!(
                            "FDT node {path} references missing interrupt provider {phandle:#x}"
                        )
                    )
                })?;
            let interrupt_cells = provider
                .get_property("#interrupt-cells")
                .and_then(Property::get_u32)
                .ok_or_else(|| {
                    ax_err_type!(
                        InvalidData,
                        std::format!(
                            "interrupt provider {phandle:#x} for {path} has no #interrupt-cells"
                        )
                    )
                })? as usize;
            let end = cursor
                .checked_add(interrupt_cells + 1)
                .filter(|end| *end <= cells.len())
                .ok_or_else(|| {
                    ax_err_type!(
                        InvalidData,
                        std::format!("FDT node {path} has truncated interrupts-extended")
                    )
                })?;
            if find_node_by_phandle(guest.inner(), phandle).is_some() {
                filtered.extend_from_slice(&cells[cursor..end]);
            }
            cursor = end;
        }

        if filtered.len() != cells.len() {
            let mut property = Property::new("interrupts-extended", std::vec![]);
            property.set_u32_ls(&filtered);
            guest.set_property(node_id, property)?;
        }
    }
    Ok(())
}

fn find_node_by_phandle(fdt: &Fdt, phandle: u32) -> Option<NodeId> {
    fdt.iter_node_ids().find(|node_id| {
        fdt.node(*node_id).is_some_and(|node| {
            node.get_property("phandle")
                .or_else(|| node.get_property("linux,phandle"))
                .and_then(Property::get_u32)
                == Some(phandle)
        })
    })
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
    crate_config: &GuestConfig,
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
    crate_config: &GuestConfig,
) -> AxVmResult {
    let patch_runtime = super::selected_guest_fdt_policy().patch_runtime;
    // SAFETY: `fdt_src` originates from `GuestDtbImage::as_bytes`, and the
    // caller supplies the exact slice length while the image remains borrowed.
    let fdt_bytes = unsafe { std::slice::from_raw_parts(fdt_src.as_ptr(), dtb_size) };
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

pub(crate) struct GuestFdtRuntimePatch<'a> {
    pub(crate) fdt_bytes: &'a [u8],
    pub(crate) memory_regions: &'a [VMMemoryRegion],
    pub(crate) devices: &'a [ResolvedFdtDevice],
    pub(crate) crate_config: &'a GuestConfig,
    pub(crate) serial_profile: crate::machine::GuestSerialProfile,
    pub(crate) serial_identity: Option<&'a crate::machine::GuestSerialFdtIdentity>,
    pub(crate) additional_serials: &'a [crate::machine::GuestSerialProfile],
    pub(crate) gic_profile: Option<&'a crate::machine::GuestGicProfile>,
    pub(crate) plic_profile: Option<&'a crate::machine::GuestPlicProfile>,
    pub(crate) timer_profile: Option<&'a crate::machine::GuestTimerProfile>,
    pub(crate) initrd_start_size: Option<(u64, u64)>,
    pub(crate) create_chosen: bool,
}

pub(crate) fn patch_guest_fdt_for_runtime(patch: GuestFdtRuntimePatch<'_>) -> AxVmResult<Vec<u8>> {
    let GuestFdtRuntimePatch {
        fdt_bytes,
        memory_regions,
        devices,
        crate_config,
        serial_profile,
        serial_identity,
        additional_serials,
        gic_profile,
        plic_profile,
        timer_profile,
        initrd_start_size,
        create_chosen,
    } = patch;
    let mut tree = FdtTree::from_bytes(fdt_bytes)?;
    let memory_specs = guest_memory_specs(memory_regions, crate_config);
    tree.rebuild_memory_nodes(&memory_specs)?;
    if create_chosen
        || initrd_start_size.is_some()
        || crate_config.kernel.cmdline.is_some()
        || tree.inner().get_by_path_id("/chosen").is_some()
    {
        tree.patch_chosen(initrd_start_size, crate_config.kernel.cmdline.as_deref())?;
    }
    super::interrupt::install_machine_interrupt_controller(
        &mut tree,
        crate_config.base.cpu_num,
        gic_profile,
        plic_profile,
    )?;
    install_resolved_fdt_devices(&mut tree, devices, gic_profile, plic_profile)?;
    super::timer::install_machine_timer(&mut tree, timer_profile)?;
    let preserved_physical_serial_selectors = crate_config
        .devices
        .passthrough
        .iter()
        .map(|device| device.path.clone())
        .collect::<Vec<_>>();
    super::serial::install_machine_serial(
        &mut tree,
        serial_profile,
        serial_identity,
        &preserved_physical_serial_selectors,
    )?;
    for serial in additional_serials {
        super::serial::install_additional_serial(&mut tree, *serial)?;
    }
    let bytes = tree.finish();
    Fdt::from_bytes(&bytes).map_err(|error| {
        ax_err_type!(InvalidData, std::format!("invalid patched FDT: {error:?}"))
    })?;
    Ok(bytes)
}

fn install_resolved_fdt_devices(
    tree: &mut FdtTree,
    devices: &[ResolvedFdtDevice],
    gic_profile: Option<&crate::machine::GuestGicProfile>,
    plic_profile: Option<&crate::machine::GuestPlicProfile>,
) -> AxVmResult {
    for device in devices {
        let path = device.registers.first().map_or_else(
            || std::format!("/{}-{}", device.node_name, device.id),
            |(base, _)| std::format!("/{}@{base:x}", device.node_name),
        );
        let node_id = tree.ensure_path(&path)?;
        tree.set_property(
            node_id,
            string_list_property("compatible", &device.compatible),
        )?;
        if !device.registers.is_empty() {
            let registers = device
                .registers
                .iter()
                .map(|(base, size)| RegInfo::new(*base, Some(*size)))
                .collect::<Vec<_>>();
            tree.inner_mut()
                .view_typed_mut(node_id)
                .ok_or_else(|| ax_err_type!(InvalidData, "new configured FDT node is missing"))?
                .set_regs(&registers);
        }
        if !device.interrupts.is_empty() {
            let mut parent = None;
            let mut cells = Vec::new();
            for interrupt in &device.interrupts {
                let binding = fdt_interrupt_binding(tree, *interrupt, gic_profile, plic_profile)?;
                if parent
                    .replace(binding.parent())
                    .is_some_and(|value| value != binding.parent())
                {
                    return Err(crate::AxVmError::invalid_config(std::format!(
                        "device {} uses multiple FDT interrupt parents",
                        device.id
                    )));
                }
                cells.extend_from_slice(binding.cells());
            }
            tree.set_property(
                node_id,
                u32_property(
                    "interrupt-parent",
                    parent.expect("nonempty interrupts have parent"),
                ),
            )?;
            tree.set_property(node_id, u32_list_property("interrupts", &cells))?;
        }
        for property in &device.properties {
            let property = match property {
                ResolvedFdtProperty::Empty(name) => Property::new(name, std::vec![]),
                ResolvedFdtProperty::U32(name, value) => u32_property(name, *value),
                ResolvedFdtProperty::String(name, value) => prop_string(name, value),
            };
            tree.set_property(node_id, property)?;
        }
        info!(
            "Adding resolved virtual-device FDT node {path} for {}",
            device.id
        );
    }
    Ok(())
}

fn string_list_property(name: &str, values: &[String]) -> Property {
    let mut bytes = Vec::new();
    for value in values {
        bytes.extend_from_slice(value.as_bytes());
        bytes.push(0);
    }
    Property::new(name, bytes)
}

fn u32_list_property(name: &str, values: &[u32]) -> Property {
    let mut property = Property::new(name, std::vec![]);
    property.set_u32_ls(values);
    property
}

fn u32_property(name: &str, value: u32) -> Property {
    u32_list_property(name, &[value])
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FdtInterruptBinding {
    GicSpi { parent: u32, cells: [u32; 3] },
    PlicSource { parent: u32, cells: [u32; 1] },
}

impl FdtInterruptBinding {
    const fn parent(self) -> u32 {
        match self {
            Self::GicSpi { parent, .. } | Self::PlicSource { parent, .. } => parent,
        }
    }

    const fn cells(&self) -> &[u32] {
        match self {
            Self::GicSpi { cells, .. } => cells,
            Self::PlicSource { cells, .. } => cells,
        }
    }
}

fn fdt_interrupt_binding(
    tree: &mut FdtTree,
    interrupt: ResolvedFdtInterrupt,
    gic_profile: Option<&crate::machine::GuestGicProfile>,
    plic_profile: Option<&crate::machine::GuestPlicProfile>,
) -> AxVmResult<FdtInterruptBinding> {
    let machine_controller = axdevice_base::InterruptControllerId::new(0);
    if interrupt.controller != machine_controller {
        return Err(crate::AxVmError::invalid_config(std::format!(
            "device FDT interrupt controller {} differs from machine controller {}",
            interrupt.controller.value(),
            machine_controller.value()
        )));
    }
    match (gic_profile, plic_profile) {
        (Some(gic), None) => {
            let parent = match gic.node_phandle {
                Some(parent) => parent,
                None => interrupt_controller_phandle(tree, FdtInterruptEncoding::GicSpi)?,
            };
            let spi = interrupt.input.checked_sub(32).ok_or_else(|| {
                ax_err_type!(InvalidData, "resolved interrupt input is not a GIC SPI")
            })?;
            let flags = match interrupt.trigger {
                InterruptTrigger::EdgeTriggered => 1,
                InterruptTrigger::LevelTriggered => 4,
            };
            Ok(FdtInterruptBinding::GicSpi {
                parent,
                cells: [0, spi, flags],
            })
        }
        (None, Some(plic)) => {
            let parent = match plic.node_phandle {
                Some(parent) => parent,
                None => interrupt_controller_phandle(tree, FdtInterruptEncoding::PlicSource)?,
            };
            if interrupt.input == 0 {
                return Err(ax_err_type!(
                    InvalidData,
                    "resolved interrupt is not a valid PLIC source"
                ));
            }
            Ok(FdtInterruptBinding::PlicSource {
                parent,
                cells: [interrupt.input],
            })
        }
        (Some(_), Some(_)) => Err(ax_err_type!(
            InvalidData,
            "device interrupt cannot select between guest GIC and PLIC"
        )),
        (None, None) => Err(ax_err_type!(
            InvalidData,
            "device interrupt requires a guest interrupt controller profile"
        )),
    }
}

#[derive(Clone, Copy)]
enum FdtInterruptEncoding {
    GicSpi,
    PlicSource,
}

fn interrupt_controller_phandle(
    tree: &mut FdtTree,
    encoding: FdtInterruptEncoding,
) -> AxVmResult<u32> {
    let controller = tree
        .inner()
        .iter_node_ids()
        .find(|node_id| {
            let Some(node) = tree.inner().node(*node_id) else {
                return false;
            };
            if node.get_property("interrupt-controller").is_none() {
                return false;
            }
            node.compatibles().any(|compatible| match encoding {
                FdtInterruptEncoding::GicSpi => compatible.contains("gic"),
                FdtInterruptEncoding::PlicSource => compatible.contains("plic"),
            })
        })
        .ok_or_else(|| {
            ax_err_type!(
                InvalidData,
                "guest FDT has no matching interrupt controller"
            )
        })?;

    if let Some(phandle) = tree
        .inner()
        .node(controller)
        .and_then(|node| {
            node.get_property("phandle")
                .or_else(|| node.get_property("linux,phandle"))
        })
        .and_then(Property::get_u32)
    {
        return Ok(phandle);
    }

    let phandle = next_phandle(tree.inner());
    tree.set_property(controller, u32_property("phandle", phandle))?;
    tree.set_property(controller, u32_property("linux,phandle", phandle))?;
    Ok(phandle)
}

fn next_phandle(fdt: &Fdt) -> u32 {
    fdt.iter_node_ids()
        .filter_map(|node_id| {
            fdt.node(node_id).and_then(|node| {
                node.get_property("phandle")
                    .or_else(|| node.get_property("linux,phandle"))
            })
        })
        .filter_map(Property::get_u32)
        .max()
        .unwrap_or(0)
        .saturating_add(1)
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
    use std::sync::Arc;

    use axdevice::*;
    use axdevice_base::{
        ControllerInputId, InterruptControllerId, InterruptSharing, InterruptTrigger,
    };
    use axvmconfig::{GuestConfig, GuestDevices, PhysicalDeviceRef};
    use fdt_edit::{Fdt, Node, Property};
    use fdt_raw::RegInfo;

    use super::{
        super::{
            device::find_all_passthrough_devices,
            tree::{FdtTree, prop_string, sanitize_bootargs},
        },
        cpu_node_id, find_node_by_phandle, initrd_range_from_image_config, need_cpu_node,
        u32_property,
    };
    use crate::{
        GuestPhysAddr,
        config::{AxVMConfig, AxVMConfigParams, HostDeviceAssignment, PhysCpuList, RamdiskInfo},
        machine::{GuestGicCpuRegion, GuestGicProfile, GuestMmioRegion, GuestPlicProfile},
    };

    fn prop_u32(name: &str, value: u32) -> Property {
        let mut prop = Property::new(name, std::vec![]);
        prop.set_u32_ls(&[value]);
        prop
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

    fn virtio_device(id: &str, base: u64, input: u32) -> super::ResolvedFdtDevice {
        super::ResolvedFdtDevice {
            id: id.into(),
            node_name: "virtio_mmio".into(),
            compatible: std::vec!["virtio,mmio".into()],
            registers: std::vec![(base, 0x200)],
            interrupts: std::vec![super::ResolvedFdtInterrupt {
                controller: axdevice_base::InterruptControllerId::new(0),
                input,
                trigger: axdevice_base::InterruptTrigger::EdgeTriggered,
            }],
            properties: std::vec![super::ResolvedFdtProperty::Empty("dma-coherent".into())],
        }
    }

    struct PlannedVirtioModel;

    impl DeviceModel for PlannedVirtioModel {
        fn requirements(&self) -> DeviceManagerResult<DeviceRequirements> {
            DeviceRequirements::new()
                .with_mmio(
                    ResourceSlot::new("registers")?,
                    0x200,
                    0x200,
                    ResourceRequest::Auto,
                )?
                .with_wired_irq(
                    ResourceSlot::new("irq")?,
                    InterruptControllerId::new(0),
                    InterruptTrigger::EdgeTriggered,
                    InterruptSharing::Exclusive,
                    ResourceRequest::Auto,
                )
        }

        fn firmware(&self) -> DeviceFirmwareSpec {
            DeviceFirmwareSpec::interfaces(
                Some(std::vec![FdtContributionSpec::Conventional(
                    FdtNodeSpec::new("virtio_mmio")
                        .with_compatible("virtio,mmio")
                        .with_register(ResourceSlot::new("registers").unwrap())
                        .with_interrupt(ResourceSlot::new("irq").unwrap()),
                )]),
                None,
            )
        }

        fn build(
            &self,
            _context: &mut DeviceBuildContext<'_>,
        ) -> DeviceManagerResult<DeviceBundle> {
            unreachable!("FDT resolution test does not build devices")
        }
    }

    #[test]
    fn graph_resolves_one_fdt_node_per_device_instance() {
        let mut builder = DeviceGraphBuilder::new();
        for id in ["blk0", "blk1"] {
            builder
                .add(DeviceNodeSpec::virtual_device(
                    DeviceNodeId::new(id).unwrap(),
                    Arc::new(PlannedVirtioModel),
                ))
                .unwrap();
        }
        let mut pools = ResourcePools::new();
        pools.add_auto_mmio(0x0a00_0000..0x0a00_1000).unwrap();
        pools
            .add_auto_controller_inputs(
                InterruptControllerId::new(0),
                ControllerInputId::new(48)..ControllerInputId::new(50),
            )
            .unwrap();
        let graph = builder.declare().unwrap().resolve(pools).unwrap();

        let devices = crate::boot::fdt::device::resolve_fdt_devices(&graph).unwrap();

        assert_eq!(devices.len(), 2);
        assert_eq!(devices[0].registers, [(0x0a00_0000, 0x200)]);
        assert_eq!(devices[0].interrupts[0].input, 48);
        assert_eq!(devices[1].registers, [(0x0a00_0200, 0x200)]);
        assert_eq!(devices[1].interrupts[0].input, 49);
    }

    fn gic_profile(phandle: u32) -> GuestGicProfile {
        GuestGicProfile {
            compatible: "arm,gic-400".into(),
            node_path: "/interrupt-controller@8000000".into(),
            node_phandle: Some(phandle),
            distributor: GuestMmioRegion {
                base: 0x0800_0000,
                length: 0x1000,
            },
            cpu_region: GuestGicCpuRegion::CpuInterface(GuestMmioRegion {
                base: 0x0801_0000,
                length: 0x2000,
            }),
            its: std::vec![],
        }
    }

    fn plic_profile(phandle: u32) -> GuestPlicProfile {
        GuestPlicProfile {
            node_path: "/soc/interrupt-controller@c000000".into(),
            node_phandle: Some(phandle),
            base: 0x0c00_0000,
            length: 0x60_0000,
        }
    }

    #[test]
    fn riscv_virtio_net_uses_one_cell_plic_interrupt_binding() {
        let mut tree = FdtTree::new();
        super::install_resolved_fdt_devices(
            &mut tree,
            &[virtio_device("virtnet0", 0x0a00_0000, 48)],
            None,
            Some(&plic_profile(9)),
        )
        .unwrap();
        let node = tree.inner().get_by_path("/virtio_mmio@a000000").unwrap();

        assert_eq!(
            node.as_node()
                .get_property("interrupt-parent")
                .unwrap()
                .get_u32(),
            Some(9)
        );
        assert_eq!(
            node.as_node()
                .get_property("interrupts")
                .unwrap()
                .get_u32_iter()
                .collect::<std::vec::Vec<_>>(),
            [48]
        );
    }

    #[test]
    fn aarch64_virtio_net_uses_three_cell_gic_interrupt_binding() {
        let mut tree = FdtTree::new();
        super::install_resolved_fdt_devices(
            &mut tree,
            &[virtio_device("virtnet0", 0x0a00_0000, 48)],
            Some(&gic_profile(7)),
            None,
        )
        .unwrap();
        let node = tree.inner().get_by_path("/virtio_mmio@a000000").unwrap();

        assert_eq!(
            node.as_node()
                .get_property("interrupt-parent")
                .unwrap()
                .get_u32(),
            Some(7)
        );
        assert_eq!(
            node.as_node()
                .get_property("interrupts")
                .unwrap()
                .get_u32_iter()
                .collect::<std::vec::Vec<_>>(),
            [0, 16, 1]
        );
    }

    #[test]
    fn riscv_virtio_blk_uses_one_cell_plic_interrupt_binding() {
        let mut tree = FdtTree::new();
        super::install_resolved_fdt_devices(
            &mut tree,
            &[virtio_device("virtblk0", 0x0a00_0200, 49)],
            None,
            Some(&plic_profile(9)),
        )
        .unwrap();
        let node = tree.inner().get_by_path("/virtio_mmio@a000200").unwrap();

        assert_eq!(
            node.as_node()
                .get_property("interrupt-parent")
                .unwrap()
                .get_u32(),
            Some(9)
        );
        assert_eq!(
            node.as_node()
                .get_property("interrupts")
                .unwrap()
                .get_u32_iter()
                .collect::<std::vec::Vec<_>>(),
            [49]
        );
    }

    #[test]
    fn aarch64_virtio_blk_uses_three_cell_gic_interrupt_binding() {
        let mut tree = FdtTree::new();
        super::install_resolved_fdt_devices(
            &mut tree,
            &[virtio_device("virtblk0", 0x0a00_0200, 49)],
            Some(&gic_profile(7)),
            None,
        )
        .unwrap();
        let node = tree.inner().get_by_path("/virtio_mmio@a000200").unwrap();

        assert_eq!(
            node.as_node()
                .get_property("interrupt-parent")
                .unwrap()
                .get_u32(),
            Some(7)
        );
        assert_eq!(
            node.as_node()
                .get_property("interrupts")
                .unwrap()
                .get_u32_iter()
                .collect::<std::vec::Vec<_>>(),
            [0, 17, 1]
        );
    }

    #[test]
    fn fdt_rejects_interrupt_controller_not_owned_by_machine_profile() {
        let mut tree = FdtTree::new();
        let mut device = virtio_device("virtblk0", 0x0a00_0200, 49);
        device.interrupts[0].controller = InterruptControllerId::new(1);

        let error =
            super::install_resolved_fdt_devices(&mut tree, &[device], Some(&gic_profile(7)), None)
                .unwrap_err();

        assert!(error.to_string().contains("interrupt controller"));
    }

    #[test]
    fn cpu_node_selection_uses_node_id_when_reg_differs() {
        let fdt = test_fdt("cpu@0=200\ncpu@100=0\ncpu@101=100");
        let selected: std::vec::Vec<_> = fdt
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
        let cfg = GuestConfig::default();

        let serial = crate::machine::current_machine_profile(1).serial;
        let patched = super::patch_guest_fdt_for_runtime(super::GuestFdtRuntimePatch {
            fdt_bytes: &dtb,
            memory_regions: &[],
            devices: &[],
            crate_config: &cfg,
            serial_profile: serial,
            serial_identity: None,
            additional_serials: &[],
            gic_profile: None,
            plic_profile: None,
            timer_profile: None,
            initrd_start_size: None,
            create_chosen: false,
        })
        .unwrap();
        let reparsed = Fdt::from_bytes(&patched).unwrap();

        assert!(reparsed.get_by_path_id("/chosen").is_none());

        let serial = crate::machine::current_machine_profile(1).serial;
        let patched = super::patch_guest_fdt_for_runtime(super::GuestFdtRuntimePatch {
            fdt_bytes: &dtb,
            memory_regions: &[],
            devices: &[],
            crate_config: &cfg,
            serial_profile: serial,
            serial_identity: None,
            additional_serials: &[],
            gic_profile: None,
            plic_profile: None,
            timer_profile: None,
            initrd_start_size: None,
            create_chosen: true,
        })
        .unwrap();
        let reparsed = Fdt::from_bytes(&patched).unwrap();

        assert!(reparsed.get_by_path_id("/chosen").is_some());
    }

    #[test]
    fn runtime_patch_adds_ivc_channel_node() {
        let mut tree = FdtTree::new();
        let intc = tree.ensure_path("/intc@8000000").unwrap();
        tree.set_property(intc, prop_string("compatible", "arm,gic-v3"))
            .unwrap();
        tree.set_property(intc, Property::new("interrupt-controller", std::vec![]))
            .unwrap();
        tree.set_property(intc, u32_property("#interrupt-cells", 3))
            .unwrap();
        let dtb = tree.finish();
        let cfg = GuestConfig::default();
        let devices = std::vec![super::ResolvedFdtDevice {
            id: "ivc0".into(),
            node_name: "ivc-channel".into(),
            compatible: std::vec!["axvisor,ivc-channel".into()],
            registers: std::vec![(0xbff0_0000, 0x1_0000)],
            interrupts: std::vec![super::ResolvedFdtInterrupt {
                controller: axdevice_base::InterruptControllerId::new(0),
                input: 60,
                trigger: axdevice_base::InterruptTrigger::EdgeTriggered,
            }],
            properties: std::vec![
                super::ResolvedFdtProperty::String("status".into(), "okay".into()),
                super::ResolvedFdtProperty::U32("axvisor,ivc-version".into(), 1),
                super::ResolvedFdtProperty::U32("axvisor,notify-irq".into(), 60),
            ],
        }];
        let serial = crate::machine::current_machine_profile(1).serial;
        let gic = gic_profile(7);

        let patched = super::patch_guest_fdt_for_runtime(super::GuestFdtRuntimePatch {
            fdt_bytes: &dtb,
            memory_regions: &[],
            devices: &devices,
            crate_config: &cfg,
            serial_profile: serial,
            serial_identity: None,
            additional_serials: &[],
            gic_profile: Some(&gic),
            plic_profile: None,
            timer_profile: None,
            initrd_start_size: None,
            create_chosen: false,
        })
        .unwrap();
        let reparsed = Fdt::from_bytes(&patched).unwrap();
        let node_id = reparsed.get_by_path_id("/ivc-channel@bff00000").unwrap();
        let node = reparsed.node(node_id).unwrap();
        let typed_node = reparsed.view_typed(node_id).unwrap();

        assert_eq!(
            node.get_property("compatible").unwrap().as_str(),
            Some("axvisor,ivc-channel")
        );
        assert_eq!(typed_node.regs()[0].address, 0xbff0_0000);
        assert_eq!(typed_node.regs()[0].size, Some(0x1_0000));
        assert_eq!(
            node.get_property("axvisor,notify-irq").unwrap().get_u32(),
            Some(60)
        );
        assert_eq!(
            node.get_property("interrupt-parent").unwrap().get_u32(),
            Some(7)
        );
        assert_eq!(
            node.get_property("interrupts")
                .unwrap()
                .get_u32_iter()
                .collect::<std::vec::Vec<_>>(),
            [0, 28, 1]
        );
    }

    #[test]
    fn generated_fdt_filters_cpu_nodes_by_unit_address() {
        let fdt = test_fdt("cpu@0=200\ncpu@100=0\ncpu@101=100");
        let cfg = GuestConfig {
            base: axvmconfig::VMBaseConfig {
                phys_cpu_ids: Some(std::vec![0x100]),
                ..Default::default()
            },
            ..Default::default()
        };
        let dtb = super::create_guest_fdt(&fdt, &[], &cfg, &[]).unwrap();
        let reparsed = Fdt::from_bytes(&dtb).unwrap();

        assert!(reparsed.get_by_path_id("/cpus/cpu@100").is_some());
        assert!(reparsed.get_by_path_id("/cpus/cpu@0").is_none());
        assert!(reparsed.get_by_path_id("/cpus/cpu@101").is_none());
    }

    #[test]
    fn generated_fdt_removes_explicitly_disabled_passthrough_subtrees() {
        let mut fdt = test_fdt("cpu@0=0");
        let soc = fdt.add_node(fdt.root_id(), Node::new("soc"));
        let pci = fdt.add_node(soc, Node::new("pci@30000000"));
        fdt.add_node(pci, Node::new("nvme@0"));
        fdt.add_node(soc, Node::new("virtio_mmio@10001000"));
        let cfg = GuestConfig {
            base: axvmconfig::VMBaseConfig {
                phys_cpu_ids: Some(std::vec![0]),
                ..Default::default()
            },
            devices: GuestDevices {
                disabled: std::vec![PhysicalDeviceRef {
                    path: "/soc/pci@30000000".into(),
                }],
                ..Default::default()
            },
            ..Default::default()
        };
        let selected = std::vec![
            "/soc/pci@30000000".into(),
            "/soc/pci@30000000/nvme@0".into(),
            "/soc/virtio_mmio@10001000".into(),
        ];
        let excluded = cfg
            .devices
            .disabled
            .iter()
            .map(|device| device.path.clone())
            .collect::<std::vec::Vec<_>>();

        let dtb = super::create_guest_fdt(&fdt, &selected, &cfg, &excluded).unwrap();
        let guest = Fdt::from_bytes(&dtb).unwrap();

        assert!(guest.get_by_path_id("/soc/pci@30000000").is_none());
        assert!(guest.get_by_path_id("/soc/virtio_mmio@10001000").is_some());
    }

    #[test]
    fn generated_fdt_keeps_psci_firmware_node() {
        let mut fdt = test_fdt("cpu@0=0");
        let psci = fdt.add_node(fdt.root_id(), Node::new("psci"));
        let mut compatible = Property::new("compatible", std::vec![]);
        compatible.set_string("arm,psci-0.2");
        fdt.node_mut(psci).unwrap().set_property(compatible);

        let cfg = GuestConfig {
            base: axvmconfig::VMBaseConfig {
                phys_cpu_ids: Some(std::vec![0]),
                ..Default::default()
            },
            ..Default::default()
        };
        let dtb = super::create_guest_fdt(&fdt, &[], &cfg, &[]).unwrap();
        let reparsed = Fdt::from_bytes(&dtb).unwrap();

        assert!(reparsed.get_by_path_id("/psci").is_some());
    }

    #[test]
    fn generated_fdt_keeps_the_host_interrupt_controller_for_a_virtual_machine() {
        let mut fdt = test_fdt("cpu@0=0\ncpu@1=1");
        for (cpu_path, phandle) in [("/cpus/cpu@0", 8), ("/cpus/cpu@1", 6)] {
            let cpu = fdt.get_by_path_id(cpu_path).unwrap();
            let intc = fdt.add_node(cpu, Node::new("interrupt-controller"));
            fdt.node_mut(intc)
                .unwrap()
                .set_property(prop_u32("#interrupt-cells", 1));
            fdt.node_mut(intc)
                .unwrap()
                .set_property(Property::new("interrupt-controller", std::vec![]));
            fdt.node_mut(intc)
                .unwrap()
                .set_property(prop_u32("phandle", phandle));
        }
        let root = fdt.root_id();
        let soc = fdt.add_node(root, Node::new("soc"));
        let plic = fdt.add_node(soc, Node::new("plic@c000000"));
        let mut compatible = Property::new("compatible", std::vec![]);
        compatible.set_string("riscv,plic0");
        fdt.node_mut(plic).unwrap().set_property(compatible);
        fdt.node_mut(plic)
            .unwrap()
            .set_property(Property::new("interrupt-controller", std::vec![]));
        fdt.node_mut(plic)
            .unwrap()
            .set_property(prop_u32("phandle", 9));
        let mut contexts = Property::new("interrupts-extended", std::vec![]);
        contexts.set_u32_ls(&[8, 11, 8, 9, 6, 11, 6, 9]);
        fdt.node_mut(plic).unwrap().set_property(contexts);
        let its = fdt.add_node(root, Node::new("its@8080000"));
        let mut compatible = Property::new("compatible", std::vec![]);
        compatible.set_string("arm,gic-v3-its");
        fdt.node_mut(its).unwrap().set_property(compatible);
        fdt.node_mut(its)
            .unwrap()
            .set_property(Property::new("msi-controller", std::vec![]));

        let cfg = GuestConfig {
            base: axvmconfig::VMBaseConfig {
                phys_cpu_ids: Some(std::vec![0]),
                ..Default::default()
            },
            ..Default::default()
        };
        let dtb = super::create_guest_fdt(&fdt, &[], &cfg, &[]).unwrap();
        let reparsed = Fdt::from_bytes(&dtb).unwrap();
        let plic = reparsed.get_by_path("/soc/plic@c000000").unwrap();
        assert!(reparsed.get_by_path_id("/its@8080000").is_some());

        assert_eq!(
            plic.as_node().get_property("phandle").unwrap().get_u32(),
            Some(9)
        );
        assert_eq!(
            plic.as_node()
                .get_property("interrupts-extended")
                .unwrap()
                .get_u32_iter()
                .collect::<std::vec::Vec<_>>(),
            [8, 11, 8, 9]
        );
    }

    #[test]
    fn orangepi_5_plus_guest_fdt_keeps_cpu_power_dependencies_resolvable() {
        let host = Fdt::from_bytes(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../os/axvisor/configs/board/orangepi-5-plus.dtb"
        )))
        .unwrap();
        let vm_cfg = AxVMConfig::new(AxVMConfigParams {
            phys_cpu_ls: PhysCpuList::new(1, Some(std::vec![0]), None),
            pass_through_devices: std::vec![HostDeviceAssignment {
                name: "/".into(),
                ..Default::default()
            }],
            ..Default::default()
        });
        let passthrough_devices = find_all_passthrough_devices(&vm_cfg, &host);
        let cfg = GuestConfig {
            base: axvmconfig::VMBaseConfig {
                phys_cpu_ids: Some(std::vec![0]),
                ..Default::default()
            },
            ..Default::default()
        };

        let dtb = super::create_guest_fdt(&host, &passthrough_devices, &cfg, &[]).unwrap();
        let guest = Fdt::from_bytes(&dtb).unwrap();
        let cpu = guest.get_by_path("/cpus/cpu@0").unwrap().as_node();

        assert!(cpu.get_property("#cooling-cells").is_some());
        assert!(cpu.get_property("dynamic-power-coefficient").is_some());
        for property_name in ["operating-points-v2", "cpu-supply"] {
            let phandle = cpu
                .get_property(property_name)
                .and_then(Property::get_u32)
                .unwrap();
            assert!(
                find_node_by_phandle(&guest, phandle).is_some(),
                "{property_name} references missing guest phandle {phandle:#x}"
            );
        }
    }
}
