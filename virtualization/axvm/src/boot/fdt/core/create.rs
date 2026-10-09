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

#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
use std::ptr::NonNull;
use std::{string::String, vec::Vec};

#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
use ax_memory_addr::MemoryAddr;
use axdevice_base::InterruptTrigger;
use axvmconfig::GuestConfig;
use fdt_edit::{Fdt, Node, NodeId, Property};
use fdt_raw::RegInfo;

use super::{
    serial::interrupt_controller_phandle,
    tree::{FdtTree, GuestMemorySpec, prop_string},
};
pub(crate) use crate::boot::fdt::device::{
    ResolvedFdtDevice, ResolvedFdtInterrupt, ResolvedFdtProperty,
};
#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
use crate::{AxVM, GuestPhysAddr, boot::images::load_vm_image_from_memory};
use crate::{
    AxVmResult, VMMemoryRegion, ax_err_type,
    machine::GuestSerialFdtInterrupt as FdtInterruptEncoding,
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
            super::interrupt::is_machine_interrupt_provider(node).then(|| fdt.path_of(node_id))
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
    // A derived guest tree must not inherit the host CPU's DVFS controls. The
    // CPU clock, OPP and regulator providers are physical host resources; the
    // guest may still use the rest of the passthrough tree, but it must not be
    // given bindings that make its cpufreq driver a second owner of those
    // resources.
    strip_cpu_power_dependencies(&mut guest_tree)?;
    prune_cpu_references(fdt, &mut guest_tree)?;
    super::disabled::apply(&mut guest_tree, crate_config)?;
    // With vCPU over-subscription (more guest vCPUs than host physical CPUs)
    // the host FDT does not carry a CPU node for every guest virtual CPU id,
    // so clone the missing ones to keep the guest SMP bootstrap functional.
    guest_tree.ensure_guest_cpu_nodes(fdt, phys_cpu_ids)?;
    Ok(guest_tree.finish())
}

fn strip_cpu_power_dependencies(guest: &mut FdtTree) -> AxVmResult {
    const HOST_POWER_PROPERTIES: &[&str] = &[
        "#cooling-cells",
        "clock-names",
        "clocks",
        "cpu-supply",
        "dynamic-power-coefficient",
        "mem-supply",
        "nvmem-cell-names",
        "nvmem-cells",
        "operating-points-v2",
        "rockchip,pvtm-freq",
        "rockchip,pvtm-low-len-sel",
        "rockchip,pvtm-voltage-sel",
    ];

    for (node_id, path) in guest.node_paths() {
        if !path.starts_with("/cpus/cpu@") || path["/cpus/cpu@".len()..].contains('/') {
            continue;
        }
        let node = guest.inner_mut().node_mut(node_id).ok_or_else(|| {
            ax_err_type!(InvalidData, "guest CPU node disappeared during filtering")
        })?;
        for name in HOST_POWER_PROPERTIES {
            node.remove_property(name);
        }
    }
    Ok(())
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

        if node_path == "/cpus/cpu-map" || node_path.starts_with("/cpus/cpu-map/") {
            return false;
        }
        if node_path == "/cpus" {
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
            || is_ancestor_of_passthrough_device(node_path, self.passthrough_device_names)
    }
}

fn is_path_or_ancestor(candidate: &str, path: &str) -> bool {
    candidate == path
        || path
            .strip_prefix(candidate)
            .is_some_and(|suffix| candidate == "/" || suffix.starts_with('/'))
}

/// Removes references to CPU providers or capabilities omitted by projection.
pub(super) fn prune_cpu_references(source: &Fdt, guest: &mut FdtTree) -> AxVmResult {
    let source_handles = super::references::phandle_index(source)?;
    let guest_handles = super::references::phandle_index(guest.inner())?;
    for source_id in source.iter_node_ids() {
        let path = source.path_of(source_id);
        let Some(id) = guest.inner().get_by_path_id(&path) else {
            continue;
        };
        for name in ["interrupts-extended", "cooling-device"] {
            // CPU replacement may rename nodes and strip their properties.
            // Only prune consumers that survived in the destination tree.
            if guest.inner().node(id).unwrap().get_property(name).is_none() {
                continue;
            }
            let Some(property) = source.node(source_id).unwrap().get_property(name) else {
                continue;
            };
            let offsets =
                super::references::reference_offsets(source, &source_handles, source_id, property)?
                    .ok_or_else(|| {
                        ax_err_type!(Unsupported, "CPU projection requires a reference binding")
                    })?;
            let cells = property.get_u32_iter().collect::<Vec<_>>();
            let mut retained = Vec::new();
            for (index, &offset) in offsets.iter().enumerate() {
                let handle = cells[offset];
                let end = offsets.get(index + 1).copied().unwrap_or(cells.len());
                let cpu_provider = source
                    .path_of(source_handles[&handle])
                    .starts_with("/cpus/cpu@");
                let provider_retained = guest_handles.get(&handle).is_some_and(|&provider| {
                    // An execution-only CPU keeps its phandle but no longer
                    // implements the cooling-device binding.
                    !cpu_provider
                        || name != "cooling-device"
                        || guest
                            .inner()
                            .node(provider)
                            .unwrap()
                            .get_property("#cooling-cells")
                            .is_some()
                });
                if provider_retained {
                    retained.extend_from_slice(&cells[offset..end]);
                } else if !cpu_provider {
                    return Err(ax_err_type!(
                        InvalidInput,
                        std::format!(
                            "{path}:{name} loses a non-CPU provider during firmware filtering"
                        )
                    ));
                }
            }
            if retained.len() == cells.len() {
                continue;
            }
            if name == "cooling-device" && retained.is_empty() {
                guest.inner_mut().remove_by_path(&path);
            } else {
                let mut property = Property::new(name, Vec::new());
                property.set_u32_ls(&retained);
                guest.set_property(id, property)?;
            }
        }
    }
    Ok(())
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

#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
pub fn update_fdt(
    fdt_src: NonNull<u8>,
    dtb_size: usize,
    vm: &mut AxVM,
    crate_config: &GuestConfig,
) -> AxVmResult {
    let patch_runtime = super::selected_guest_fdt_policy().patch_runtime;
    // SAFETY: `fdt_src` originates from `GuestDtbImage::as_bytes`, and the
    // caller supplies the exact slice length while the image remains borrowed.
    let fdt_bytes = unsafe { std::slice::from_raw_parts(fdt_src.as_ptr(), dtb_size) };
    let new_fdt_bytes = patch_runtime(fdt_bytes, &*vm, crate_config)?;

    load_patched_fdt(vm, new_fdt_bytes)
}

#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
fn load_patched_fdt(vm: &mut AxVM, new_fdt_bytes: Vec<u8>) -> AxVmResult {
    let dest_addr = calculate_dtb_load_addr(&mut *vm, new_fdt_bytes.len())?;
    debug!(
        "New FDT will be loaded at {:x}, size: 0x{:x}",
        dest_addr,
        new_fdt_bytes.len()
    );
    load_vm_image_from_memory(&new_fdt_bytes, dest_addr, &mut *vm)?;
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
    super::disabled::validate_runtime(tree.inner(), crate_config)?;
    super::reserved::validate_allocated(tree.inner(), memory_regions)?;
    tree.validate_phandles()?;
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

#[derive(Clone, Debug, Eq, PartialEq)]
enum FdtInterruptBinding {
    GicSpi { parent: u32, cells: Vec<u32> },
    PlicSource { parent: u32, cells: Vec<u32> },
}

impl FdtInterruptBinding {
    const fn parent(&self) -> u32 {
        match self {
            Self::GicSpi { parent, .. } | Self::PlicSource { parent, .. } => *parent,
        }
    }

    fn cells(&self) -> &[u32] {
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
        (Some(_), None) => {
            let parent = interrupt_controller_phandle(tree, FdtInterruptEncoding::GicSpi)?;
            let spi = interrupt.input.checked_sub(32).ok_or_else(|| {
                ax_err_type!(InvalidData, "resolved interrupt input is not a GIC SPI")
            })?;
            let flags = match interrupt.trigger {
                InterruptTrigger::EdgeTriggered => 1,
                InterruptTrigger::LevelTriggered => 4,
            };
            let mut cells = std::vec![0, spi, flags];
            match tree.interrupt_cells(parent)? {
                3 => {}
                4 => cells.push(0),
                count => {
                    return Err(ax_err_type!(
                        InvalidData,
                        std::format!(
                            "guest GIC uses unsupported {count}-cell interrupt specifiers"
                        )
                    ));
                }
            }
            Ok(FdtInterruptBinding::GicSpi { parent, cells })
        }
        (None, Some(_)) => {
            let parent = interrupt_controller_phandle(tree, FdtInterruptEncoding::PlicSource)?;
            if interrupt.input == 0 {
                return Err(ax_err_type!(
                    InvalidData,
                    "resolved interrupt is not a valid PLIC source"
                ));
            }
            Ok(FdtInterruptBinding::PlicSource {
                parent,
                cells: std::vec![interrupt.input],
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

#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
pub(crate) fn calculate_dtb_load_addr(vm: &mut AxVM, fdt_size: usize) -> AxVmResult<GuestPhysAddr> {
    const MB: usize = 1024 * 1024;

    let main_memory =
        vm.memory_regions().first().cloned().ok_or_else(|| {
            ax_err_type!(InvalidInput, "VM has no memory region for DTB placement")
        })?;

    let dtb_addr = {
        let config = vm.config_mut();
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
    };

    Ok(dtb_addr)
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, sync::Arc};

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
        initrd_range_from_image_config, u32_property,
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

    fn install_gic_provider(tree: &mut FdtTree, phandle: u32, interrupt_cells: u32) {
        let controller = tree.ensure_path("/interrupt-controller@8000000").unwrap();
        tree.set_property(
            controller,
            Property::new("interrupt-controller", std::vec![]),
        )
        .unwrap();
        tree.set_property(controller, prop_string("compatible", "arm,gic-400"))
            .unwrap();
        tree.set_property(controller, prop_u32("phandle", phandle))
            .unwrap();
        tree.set_property(controller, prop_u32("#interrupt-cells", interrupt_cells))
            .unwrap();
    }

    fn install_plic_provider(tree: &mut FdtTree) {
        let controller = tree
            .ensure_path("/soc/interrupt-controller@c000000")
            .unwrap();
        tree.set_property(
            controller,
            Property::new("interrupt-controller", std::vec![]),
        )
        .unwrap();
        tree.set_property(controller, prop_string("compatible", "riscv,plic0"))
            .unwrap();
        tree.set_property(controller, prop_u32("phandle", 9))
            .unwrap();
        tree.set_property(controller, prop_u32("#interrupt-cells", 1))
            .unwrap();
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
    fn resolved_device_uses_guest_plic_identity_and_one_cell_interrupt() {
        let mut tree = FdtTree::new();
        install_plic_provider(&mut tree);
        super::install_resolved_fdt_devices(
            &mut tree,
            &[virtio_device("device0", 0x0a00_0000, 48)],
            None,
            Some(&plic_profile(99)),
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
    fn resolved_device_uses_guest_gic_identity_and_three_cell_interrupt() {
        let mut tree = FdtTree::new();
        install_gic_provider(&mut tree, 7, 3);
        super::install_resolved_fdt_devices(
            &mut tree,
            &[virtio_device("device0", 0x0a00_0000, 48)],
            Some(&gic_profile(99)),
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

    fn runtime_controller_fixture() -> (
        Vec<u8>,
        Option<crate::machine::GuestGicProfile>,
        Option<crate::machine::GuestPlicProfile>,
    ) {
        let machine = crate::machine::current_machine_profile(1);
        let mut tree = FdtTree::new();
        let (gic, plic, path, compatible, cells) = if let Some(mut plic) = machine.plic {
            plic.node_phandle = Some(7);
            let path = plic.node_path.clone();
            (None, Some(plic), path, "riscv,plic0", 1)
        } else {
            let gic = gic_profile(7);
            let path = gic.node_path.clone();
            (Some(gic), None, path, "arm,gic-v3", 4)
        };
        let intc = tree.ensure_path(&path).unwrap();
        tree.set_property(intc, prop_string("compatible", compatible))
            .unwrap();
        tree.set_property(intc, Property::new("interrupt-controller", std::vec![]))
            .unwrap();
        tree.set_property(intc, u32_property("#interrupt-cells", cells))
            .unwrap();
        (tree.finish(), gic, plic)
    }

    #[test]
    fn runtime_patch_can_leave_missing_chosen_for_host_copy() {
        let (dtb, gic, plic) = runtime_controller_fixture();
        let cfg = GuestConfig::default();

        // A port UART has no FDT stdout node; isolate the chosen creation policy
        // from the MMIO console's independent requirement to publish stdout.
        let serial = crate::machine::GuestSerialProfile {
            model: crate::machine::GuestSerialModel::Uart16550,
            transport: crate::machine::GuestSerialTransport::Port {
                base: 0x3f8,
                length: 8,
            },
            irq: 4,
            clock_hz: 1_843_200,
        };
        let patched = super::patch_guest_fdt_for_runtime(super::GuestFdtRuntimePatch {
            fdt_bytes: &dtb,
            memory_regions: &[],
            devices: &[],
            crate_config: &cfg,
            serial_profile: serial,
            serial_identity: None,
            additional_serials: &[],
            gic_profile: gic.as_ref(),
            plic_profile: plic.as_ref(),
            timer_profile: None,
            initrd_start_size: None,
            create_chosen: false,
        })
        .unwrap();
        let reparsed = Fdt::from_bytes(&patched).unwrap();

        assert!(reparsed.get_by_path_id("/chosen").is_none());

        let patched = super::patch_guest_fdt_for_runtime(super::GuestFdtRuntimePatch {
            fdt_bytes: &dtb,
            memory_regions: &[],
            devices: &[],
            crate_config: &cfg,
            serial_profile: serial,
            serial_identity: None,
            additional_serials: &[],
            gic_profile: gic.as_ref(),
            plic_profile: plic.as_ref(),
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
        let (dtb, gic, plic) = runtime_controller_fixture();
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

        let patched = super::patch_guest_fdt_for_runtime(super::GuestFdtRuntimePatch {
            fdt_bytes: &dtb,
            memory_regions: &[],
            devices: &devices,
            crate_config: &cfg,
            serial_profile: serial,
            serial_identity: None,
            additional_serials: &[],
            gic_profile: gic.as_ref(),
            plic_profile: plic.as_ref(),
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
            if plic.is_some() {
                std::vec![60]
            } else {
                std::vec![0, 28, 1, 0]
            }
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
    fn generated_fdt_exclusion_overrides_default_root_passthrough() {
        let mut host = test_fdt("cpu@0=0");
        let soc = host.add_node(host.root_id(), Node::new("soc"));
        let pci = host.add_node(soc, Node::new("pci@30000000"));
        host.add_node(pci, Node::new("nvme@0"));
        host.add_node(soc, Node::new("virtio_mmio@10001000"));
        let vm_cfg = AxVMConfig::new(AxVMConfigParams {
            phys_cpu_ls: PhysCpuList::new(1, Some(std::vec![0]), None),
            pass_through_devices: std::vec![HostDeviceAssignment {
                name: "/".into(),
                ..Default::default()
            }],
            excluded_devices: std::vec![std::vec!["/soc/pci@30000000".into()]],
            ..Default::default()
        });
        let passthrough_devices = find_all_passthrough_devices(&vm_cfg, &host).unwrap();
        let excluded_device_paths = vm_cfg
            .excluded_devices()
            .iter()
            .flatten()
            .cloned()
            .collect::<Vec<_>>();

        let cfg = GuestConfig {
            base: axvmconfig::VMBaseConfig {
                phys_cpu_ids: Some(std::vec![0]),
                ..Default::default()
            },
            ..Default::default()
        };

        let dtb =
            super::create_guest_fdt(&host, &passthrough_devices, &cfg, &excluded_device_paths)
                .unwrap();
        let guest = Fdt::from_bytes(&dtb).unwrap();

        assert!(guest.get_by_path_id("/soc").is_some());
        assert!(guest.get_by_path_id("/soc/virtio_mmio@10001000").is_some());
        assert!(guest.get_by_path_id("/soc/pci@30000000").is_none());
        assert!(guest.get_by_path_id("/soc/pci@30000000/nvme@0").is_none());
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
    fn generated_fdt_selects_peripheral_interrupt_controllers_by_configuration() {
        let mut fdt = test_fdt("cpu@0=0");
        let root = fdt.root_id();
        for (name, compatible) in [
            ("machine", "riscv,plic0"),
            ("interrupt-controller@1000", "brcm,bcm2711-l2-intc"),
            ("peripheral", "example,gic-peripheral"),
        ] {
            let id = fdt.add_node(root, Node::new(name));
            fdt.node_mut(id)
                .unwrap()
                .set_property(super::super::tree::prop_string("compatible", compatible));
            fdt.node_mut(id)
                .unwrap()
                .set_property(Property::new("interrupt-controller", std::vec![]));
        }
        let machine = fdt.get_by_path_id("/machine").unwrap();
        fdt.node_mut(machine)
            .unwrap()
            .set_property(prop_u32("phandle", 9));
        fdt.node_mut(machine)
            .unwrap()
            .set_property(prop_u32("#interrupt-cells", 1));
        let peripheral = fdt.get_by_path_id("/interrupt-controller@1000").unwrap();
        fdt.node_mut(peripheral)
            .unwrap()
            .set_property(prop_u32("interrupt-parent", 9));
        let mut interrupts = Property::new("interrupts", std::vec![]);
        interrupts.set_u32_ls(&[45]);
        fdt.node_mut(peripheral).unwrap().set_property(interrupts);
        let mut vm_cfg = AxVMConfig::new(AxVMConfigParams {
            pass_through_devices: std::vec![HostDeviceAssignment {
                name: "/interrupt-controller@1000".into(),
                ..Default::default()
            }],
            ..Default::default()
        });
        super::super::parser::parse_vm_interrupt(
            &mut vm_cfg,
            &GuestConfig::default(),
            fdt.encode().as_ref(),
        )
        .unwrap();
        assert!(
            vm_cfg
                .pass_through_irqs()
                .iter()
                .any(|irq| irq.source == 45)
        );
        let mut cfg = GuestConfig::default();
        cfg.base.phys_cpu_ids = Some(std::vec![0]);
        let paths: [std::string::String; 2] =
            ["/interrupt-controller@1000".into(), "/peripheral".into()];
        let bytes = super::create_guest_fdt(&fdt, &[], &cfg, &[]).unwrap();
        let guest = Fdt::from_bytes(&bytes).unwrap();
        assert!(guest.get_by_path_id("/machine").is_some());
        for path in &paths {
            assert!(guest.get_by_path_id(path).is_none());
        }
        let bytes = super::create_guest_fdt(&fdt, &paths, &cfg, &[]).unwrap();
        let guest = Fdt::from_bytes(&bytes).unwrap();
        for path in &paths {
            assert!(guest.get_by_path_id(path).is_some());
            cfg.devices
                .disabled
                .push(PhysicalDeviceRef { path: path.clone() });
        }
        let bytes = super::create_guest_fdt(&fdt, &paths, &cfg, &paths).unwrap();
        let guest = Fdt::from_bytes(&bytes).unwrap();
        for path in &paths {
            assert!(guest.get_by_path_id(path).is_none());
        }
        cfg.devices.disabled.push(PhysicalDeviceRef {
            path: "/machine".into(),
        });
        assert!(super::create_guest_fdt(&fdt, &paths, &cfg, &paths).is_err());
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
    fn orangepi_5_plus_guest_fdt_does_not_expose_host_cpu_power_controls() {
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
        let passthrough_devices = find_all_passthrough_devices(&vm_cfg, &host).unwrap();
        let cfg = GuestConfig {
            base: axvmconfig::VMBaseConfig {
                phys_cpu_ids: Some(std::vec![0]),
                ..Default::default()
            },
            ..Default::default()
        };

        let dtb = super::create_guest_fdt(&host, &passthrough_devices, &cfg, &[]).unwrap();
        let guest = Fdt::from_bytes(&dtb).unwrap();
        // Resource extraction reparses the filtered firmware before installing devices.
        find_all_passthrough_devices(&vm_cfg, &guest).unwrap();
        let cpu = guest.get_by_path("/cpus/cpu@0").unwrap().as_node();

        for property_name in [
            "#cooling-cells",
            "clocks",
            "cpu-supply",
            "dynamic-power-coefficient",
            "mem-supply",
            "nvmem-cells",
            "operating-points-v2",
        ] {
            assert!(
                cpu.get_property(property_name).is_none(),
                "host CPU control property {property_name} leaked into guest FDT"
            );
        }
    }

    /// Builds a host FDT that advertises its CPUs with `phandle` and a
    /// `/cpus/cpu-map` referencing them.
    fn host_fdt_with_cpu_map_phandles() -> Fdt {
        let mut fdt = Fdt::new();
        let root = fdt.root_id();
        let cpus = fdt.add_node(root, Node::new("cpus"));
        fdt.node_mut(cpus)
            .unwrap()
            .set_property(prop_u32("#address-cells", 1));
        fdt.node_mut(cpus)
            .unwrap()
            .set_property(prop_u32("#size-cells", 0));
        let cpu_map = fdt.add_node(cpus, Node::new("cpu-map"));

        for (cluster_name, cpu_name, reg, phandle) in [
            ("cluster0", "cpu@0", 0u64, 7u32),
            ("cluster1", "cpu@100", 0x100, 8),
        ] {
            let cluster = fdt.add_node(cpu_map, Node::new(cluster_name));
            let core = fdt.add_node(cluster, Node::new("core0"));
            fdt.node_mut(core)
                .unwrap()
                .set_property(prop_u32("cpu", phandle));

            let cpu = fdt.add_node(cpus, Node::new(cpu_name));
            let node = fdt.node_mut(cpu).unwrap();
            node.set_property(prop_string("device_type", "cpu"));
            node.set_property(prop_string("enable-method", "psci"));
            node.set_property(prop_u32("phandle", phandle));
            fdt.view_typed_mut(cpu)
                .unwrap()
                .set_regs(&[RegInfo::new(reg, None)]);
        }

        fdt
    }

    fn phandle_owners(fdt: &Fdt) -> std::vec::Vec<(u32, std::string::String)> {
        fdt.iter_node_ids()
            .filter_map(|node_id| {
                let node = fdt.node(node_id)?;
                let phandle = node
                    .get_property("phandle")
                    .or_else(|| node.get_property("linux,phandle"))
                    .and_then(Property::get_u32)?;
                Some((phandle, fdt.path_of(node_id)))
            })
            .collect()
    }

    fn cpu_phandle(fdt: &Fdt, path: &str) -> u32 {
        fdt.get_by_path(path)
            .unwrap_or_else(|| panic!("{path} is missing"))
            .as_node()
            .get_property("phandle")
            .and_then(Property::get_u32)
            .unwrap_or_else(|| panic!("{path} has no phandle"))
    }

    #[test]
    fn generated_fdt_over_subscription_keeps_cpu_phandles_unique_and_drops_host_cpu_map() {
        let host = host_fdt_with_cpu_map_phandles();
        let cfg = GuestConfig {
            base: axvmconfig::VMBaseConfig {
                phys_cpu_ids: Some(std::vec![0, 1, 2]),
                ..Default::default()
            },
            ..Default::default()
        };

        let dtb = super::create_guest_fdt(&host, &[], &cfg, &[]).unwrap();
        let guest = Fdt::from_bytes(&dtb).unwrap();

        // `cpu@1` and `cpu@2` have no host CPU node, so they are cloned from
        // `cpu@0` and must not inherit its phandle.
        let mut seen = BTreeMap::new();
        for (phandle, path) in phandle_owners(&guest) {
            let previous = seen.insert(phandle, path.clone());
            assert!(
                previous.is_none(),
                "phandle {phandle:#x} is defined by both {previous:?} and {path}"
            );
        }
        assert_eq!(cpu_phandle(&guest, "/cpus/cpu@0"), 7);
        let first = cpu_phandle(&guest, "/cpus/cpu@1");
        let second = cpu_phandle(&guest, "/cpus/cpu@2");
        assert!(
            first > 8 && second > 8,
            "clones reused a host phandle: {first:#x} and {second:#x}"
        );
        assert_ne!(first, second);

        // CPU projection drops the host-only `cpu-map`; guest startup
        // enumerates the projected CPU nodes by their `reg` values instead.
        assert!(guest.get_by_path_id("/cpus/cpu-map").is_none());
    }
}
