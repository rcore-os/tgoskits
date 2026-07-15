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

use alloc::{format, string::String, vec::Vec};
use core::ptr::NonNull;

use ax_memory_addr::MemoryAddr;
use axvmconfig::{AxVMCrateConfig, EmulatedDeviceType};
use fdt_edit::{Fdt, Node, NodeId, Property};
use fdt_raw::RegInfo;

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

    let mut guest_tree = FdtTree::clone_filtered(fdt, |node_id, path, node| {
        should_keep_generated_node(
            fdt,
            node_id,
            path,
            node,
            passthrough_device_names,
            phys_cpu_ids,
        )
    })?;
    if super::selected_guest_fdt_policy().describe_aarch64_consoles {
        append_aarch64_emulated_console_nodes(&mut guest_tree, crate_config, Some(fdt))?;
    }
    Ok(guest_tree.finish())
}

#[derive(Clone, Copy)]
enum ConsoleModel {
    Pl011,
    Uart16550,
}

struct ConsoleFdtSpec<'a> {
    device: &'a axvmconfig::EmulatedDeviceConfig,
    model: ConsoleModel,
    spi: u32,
}

fn append_aarch64_emulated_console_nodes(
    tree: &mut FdtTree,
    crate_config: &AxVMCrateConfig,
    existing_fdt: Option<&Fdt>,
) -> AxVmResult {
    let consoles = validate_console_specs(crate_config)?;
    let Some(primary) = consoles.first() else {
        return Ok(());
    };
    let clock_phandle = consoles
        .iter()
        .any(|console| matches!(console.model, ConsoleModel::Pl011))
        .then(|| ensure_pl011_clock(tree, existing_fdt))
        .transpose()?;
    let root = tree.inner().root_id();
    for console in &consoles {
        match console.model {
            ConsoleModel::Pl011 => {
                let phandle = clock_phandle.ok_or_else(|| {
                    ax_err_type!(InvalidData, "PL011 console clock was not prepared")
                })?;
                append_pl011_node(tree, root, console, phandle)?;
            }
            ConsoleModel::Uart16550 => append_16550_node(tree, root, console)?,
        }
    }
    let (stdout_path, earlycon, console) = console_boot_config(primary);
    patch_chosen_console(
        tree,
        existing_fdt,
        crate_config.kernel.cmdline.as_deref(),
        &stdout_path,
        &earlycon,
        console,
    )
}

fn validate_console_specs(crate_config: &AxVMCrateConfig) -> AxVmResult<Vec<ConsoleFdtSpec<'_>>> {
    crate_config
        .devices
        .emu_devices
        .iter()
        .filter(|device| device.emu_type == EmulatedDeviceType::Console)
        .map(|device| {
            let model = match device.cfg_list.as_slice() {
                [] => ConsoleModel::Pl011,
                [1] => ConsoleModel::Uart16550,
                _ => {
                    return Err(ax_err_type!(
                        InvalidInput,
                        format!(
                            "unsupported console subtype configuration: {:?}",
                            device.cfg_list
                        )
                    ));
                }
            };
            if !(32..1020).contains(&device.irq_id) {
                return Err(ax_err_type!(
                    InvalidInput,
                    format!("console IRQ {} is not a valid GIC SPI INTID", device.irq_id)
                ));
            }
            Ok(ConsoleFdtSpec {
                device,
                model,
                spi: (device.irq_id - 32) as u32,
            })
        })
        .collect()
}

fn append_pl011_node(
    tree: &mut FdtTree,
    root: NodeId,
    console: &ConsoleFdtSpec<'_>,
    clock_phandle: u32,
) -> AxVmResult {
    let device = console.device;
    let node_id = tree.add_node(root, Node::new(&format!("pl011@{:x}", device.base_gpa)));
    tree.set_property(
        node_id,
        prop_string_list("compatible", &["arm,pl011", "arm,primecell"]),
    )?;
    set_console_regs(tree, node_id, device)?;
    tree.set_property(node_id, prop_u32_array("interrupts", &[0, console.spi, 4]))?;
    tree.set_property(
        node_id,
        prop_u32_array("clocks", &[clock_phandle, clock_phandle]),
    )?;
    tree.set_property(
        node_id,
        prop_string_list("clock-names", &["uartclk", "apb_pclk"]),
    )?;
    tree.set_property(node_id, prop_u32("clock-frequency", 24_000_000))?;
    tree.set_property(node_id, super::tree::prop_string("status", "okay"))
}

fn append_16550_node(tree: &mut FdtTree, root: NodeId, console: &ConsoleFdtSpec<'_>) -> AxVmResult {
    let device = console.device;
    let node_id = tree.add_node(root, Node::new(&format!("serial@{:x}", device.base_gpa)));
    tree.set_property(node_id, super::tree::prop_string("compatible", "ns16550a"))?;
    set_console_regs(tree, node_id, device)?;
    tree.set_property(node_id, prop_u32_array("interrupts", &[0, console.spi, 4]))?;
    tree.set_property(node_id, prop_u32("clock-frequency", 1_843_200))?;
    tree.set_property(node_id, prop_u32("current-speed", 115_200))?;
    tree.set_property(node_id, prop_u32("reg-shift", 0))?;
    tree.set_property(node_id, prop_u32("reg-io-width", 1))?;
    tree.set_property(node_id, super::tree::prop_string("status", "okay"))
}

fn set_console_regs(
    tree: &mut FdtTree,
    node_id: NodeId,
    device: &axvmconfig::EmulatedDeviceConfig,
) -> AxVmResult {
    tree.inner_mut()
        .view_typed_mut(node_id)
        .ok_or_else(|| ax_err_type!(InvalidData, "new console node is missing"))?
        .set_regs(&[RegInfo::new(
            device.base_gpa as u64,
            Some(device.length as u64),
        )]);
    Ok(())
}

fn ensure_pl011_clock(tree: &mut FdtTree, existing_fdt: Option<&Fdt>) -> AxVmResult<u32> {
    if let Some(phandle) = existing_pl011_clock_phandle(tree) {
        return Ok(phandle);
    }
    let phandle = unused_phandle(tree.inner(), existing_fdt)?;
    let root = tree.inner().root_id();
    let node_name = if tree.inner().get_by_path_id("/pl011-clock").is_none() {
        String::from("pl011-clock")
    } else {
        format!("pl011-clock-{phandle:x}")
    };
    let clock_id = tree.add_node(root, Node::new(&node_name));
    tree.set_property(
        clock_id,
        super::tree::prop_string("compatible", "fixed-clock"),
    )?;
    tree.set_property(clock_id, prop_u32("#clock-cells", 0))?;
    tree.set_property(clock_id, prop_u32("clock-frequency", 24_000_000))?;
    tree.set_property(
        clock_id,
        super::tree::prop_string("clock-output-names", "clk24mhz"),
    )?;
    tree.set_property(clock_id, prop_u32("phandle", phandle))?;
    Ok(phandle)
}

fn existing_pl011_clock_phandle(tree: &FdtTree) -> Option<u32> {
    let node = tree.inner().get_by_path("/apb-pclk")?.as_node();
    node.get_property("phandle")
        .or_else(|| node.get_property("linux,phandle"))
        .and_then(Property::get_u32)
}

fn unused_phandle(tree: &Fdt, existing_fdt: Option<&Fdt>) -> AxVmResult<u32> {
    (1..=u32::MAX)
        .find(|candidate| {
            !fdt_uses_phandle(tree, *candidate)
                && existing_fdt.is_none_or(|fdt| !fdt_uses_phandle(fdt, *candidate))
        })
        .ok_or_else(|| ax_err_type!(InvalidData, "FDT has no free phandle for the PL011 clock"))
}

fn fdt_uses_phandle(fdt: &Fdt, candidate: u32) -> bool {
    fdt.iter_node_ids().any(|node_id| {
        fdt.node(node_id).is_some_and(|node| {
            node.get_property("phandle")
                .or_else(|| node.get_property("linux,phandle"))
                .and_then(Property::get_u32)
                == Some(candidate)
        })
    })
}

fn console_boot_config(primary: &ConsoleFdtSpec<'_>) -> (String, String, &'static str) {
    let base = primary.device.base_gpa;
    match primary.model {
        ConsoleModel::Pl011 => (
            format!("/pl011@{base:x}"),
            format!("pl011,mmio32,0x{base:x}"),
            "ttyAMA0,115200",
        ),
        ConsoleModel::Uart16550 => (
            format!("/serial@{base:x}"),
            format!("uart8250,mmio,0x{base:x},115200"),
            "ttyS0,115200",
        ),
    }
}

fn patch_chosen_console(
    tree: &mut FdtTree,
    fallback_fdt: Option<&Fdt>,
    configured_bootargs: Option<&str>,
    stdout_path: &str,
    earlycon: &str,
    console: &'static str,
) -> AxVmResult {
    let chosen_id = tree.ensure_path("/chosen")?;
    let bootargs = configured_bootargs
        .or_else(|| {
            tree.inner()
                .node(chosen_id)
                .and_then(|node| node.get_property("bootargs"))
                .and_then(Property::as_str)
        })
        .or_else(|| chosen_bootargs(fallback_fdt))
        .map(|args| rewrite_console_bootargs(args, earlycon, console))
        .unwrap_or_else(|| format!("earlycon={earlycon} console={console}"));
    tree.set_property(
        chosen_id,
        super::tree::prop_string("stdout-path", stdout_path),
    )?;
    tree.set_property(chosen_id, super::tree::prop_string("bootargs", &bootargs))?;
    let aliases_id = tree.ensure_path("/aliases")?;
    tree.set_property(aliases_id, super::tree::prop_string("serial0", stdout_path))
}

fn chosen_bootargs(fdt: Option<&Fdt>) -> Option<&str> {
    fdt.and_then(|fdt| fdt.get_by_path("/chosen"))
        .and_then(|node| node.as_node().get_property("bootargs"))
        .and_then(Property::as_str)
}

fn rewrite_console_bootargs(bootargs: &str, earlycon: &str, console: &'static str) -> String {
    let (kernel_args, init_suffix) = split_init_arguments(bootargs);
    let mut rewritten = kernel_args
        .split_whitespace()
        .filter(|arg| !arg.starts_with("console=") && !arg.starts_with("earlycon="))
        .collect::<Vec<_>>()
        .join(" ");
    push_bootarg(&mut rewritten, &format!("earlycon={earlycon}"));
    push_bootarg(&mut rewritten, &format!("console={console}"));
    if let Some(suffix) = init_suffix {
        push_bootarg(&mut rewritten, "--");
        rewritten.push_str(suffix);
    }
    rewritten
}

fn split_init_arguments(bootargs: &str) -> (&str, Option<&str>) {
    let mut offset = 0;
    for token in bootargs.split_whitespace() {
        let Some(relative) = bootargs[offset..].find(token) else {
            return (bootargs, None);
        };
        let start = offset + relative;
        let end = start + token.len();
        if token == "--" {
            return (bootargs[..start].trim_end(), Some(&bootargs[end..]));
        }
        offset = end;
    }
    (bootargs, None)
}

fn push_bootarg(bootargs: &mut String, arg: &str) {
    if !bootargs.is_empty() {
        bootargs.push(' ');
    }
    bootargs.push_str(arg);
}

fn prop_u32(name: &str, value: u32) -> Property {
    prop_u32_array(name, &[value])
}

fn prop_u32_array(name: &str, values: &[u32]) -> Property {
    let mut property = Property::new(name, Vec::new());
    property.set_u32_ls(values);
    property
}

fn prop_string_list(name: &str, values: &[&str]) -> Property {
    let mut property = Property::new(name, Vec::new());
    property.set_string_ls(values);
    property
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
    use axvmconfig::{
        AxVMCrateConfig, EmulatedDeviceConfig, EmulatedDeviceType, VMBaseConfig, VMDevicesConfig,
    };
    use fdt_edit::{Fdt, Node, Property};
    use fdt_raw::RegInfo;

    use super::{
        super::tree::sanitize_bootargs, cpu_node_id, initrd_range_from_image_config, need_cpu_node,
        rewrite_console_bootargs,
    };
    use crate::{GuestPhysAddr, config::RamdiskInfo};

    fn prop_u32(name: &str, value: u32) -> Property {
        let mut prop = Property::new(name, alloc::vec![]);
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

    fn console_config(
        base: usize,
        irq_id: usize,
        cfg_list: alloc::vec::Vec<usize>,
    ) -> AxVMCrateConfig {
        AxVMCrateConfig {
            base: VMBaseConfig {
                phys_cpu_ids: Some(alloc::vec![]),
                ..Default::default()
            },
            kernel: axvmconfig::VMKernelConfig {
                cmdline: Some("root=/dev/vda console=ttyS9 earlycon=old -- -n -l /bin/sh".into()),
                ..Default::default()
            },
            devices: VMDevicesConfig {
                emu_devices: alloc::vec![EmulatedDeviceConfig {
                    name: "console".into(),
                    base_gpa: base,
                    length: 0x1000,
                    irq_id,
                    emu_type: EmulatedDeviceType::Console,
                    cfg_list,
                }],
                ..Default::default()
            },
        }
    }

    fn console_source_fdt() -> Fdt {
        let mut fdt = Fdt::new();
        let root = fdt.root_id();
        fdt.node_mut(root)
            .unwrap()
            .set_property(prop_u32("#address-cells", 2));
        fdt.node_mut(root)
            .unwrap()
            .set_property(prop_u32("#size-cells", 2));
        fdt
    }

    #[test]
    fn rewrite_console_bootargs_preserves_init_arguments() {
        assert_eq!(
            rewrite_console_bootargs(
                "root=/dev/vda console=ttyAMA0 -- -n -l /bin/sh",
                "pl011,mmio32,0x9000000",
                "ttyAMA0,115200",
            ),
            "root=/dev/vda earlycon=pl011,mmio32,0x9000000 console=ttyAMA0,115200 -- -n -l /bin/sh"
        );
    }

    #[test]
    fn generated_pl011_console_has_linux_bindings_and_boot_paths() {
        let fdt = console_source_fdt();
        let cfg = console_config(0x0900_0000, 33, alloc::vec![]);
        let dtb = super::create_guest_fdt(&fdt, &[], &cfg).unwrap();
        let reparsed = Fdt::from_bytes(&dtb).unwrap();
        let node = reparsed.get_by_path("/pl011@9000000").unwrap().as_node();

        assert_eq!(
            node.get_property("compatible")
                .unwrap()
                .as_str_iter()
                .collect::<alloc::vec::Vec<_>>(),
            ["arm,pl011", "arm,primecell"]
        );
        let regs = reparsed
            .view_typed(reparsed.get_by_path_id("/pl011@9000000").unwrap())
            .unwrap()
            .regs();
        assert_eq!((regs[0].address, regs[0].size), (0x0900_0000, Some(0x1000)));
        assert_eq!(
            node.get_property("interrupts")
                .unwrap()
                .get_u32_iter()
                .collect::<alloc::vec::Vec<_>>(),
            [0, 1, 4]
        );
        assert_eq!(
            node.get_property("clock-names")
                .unwrap()
                .as_str_iter()
                .collect::<alloc::vec::Vec<_>>(),
            ["uartclk", "apb_pclk"]
        );
        assert_eq!(node.get_property("status").unwrap().as_str(), Some("okay"));
        assert_eq!(
            reparsed
                .get_by_path("/aliases")
                .unwrap()
                .as_node()
                .get_property("serial0")
                .unwrap()
                .as_str(),
            Some("/pl011@9000000")
        );
        let chosen = reparsed.get_by_path("/chosen").unwrap().as_node();
        assert_eq!(
            chosen.get_property("stdout-path").unwrap().as_str(),
            Some("/pl011@9000000")
        );
        assert_eq!(
            chosen.get_property("bootargs").unwrap().as_str(),
            Some(
                "root=/dev/vda earlycon=pl011,mmio32,0x9000000 console=ttyAMA0,115200 -- -n -l \
                 /bin/sh"
            )
        );
    }

    #[test]
    fn generated_16550_console_has_linux_bindings() {
        let fdt = console_source_fdt();
        let cfg = console_config(0x0901_0000, 48, alloc::vec![1]);
        let dtb = super::create_guest_fdt(&fdt, &[], &cfg).unwrap();
        let reparsed = Fdt::from_bytes(&dtb).unwrap();
        let node = reparsed.get_by_path("/serial@9010000").unwrap().as_node();

        assert_eq!(
            node.get_property("compatible").unwrap().as_str(),
            Some("ns16550a")
        );
        assert_eq!(
            node.get_property("interrupts")
                .unwrap()
                .get_u32_iter()
                .collect::<alloc::vec::Vec<_>>(),
            [0, 16, 4]
        );
        assert_eq!(
            node.get_property("clock-frequency").unwrap().get_u32(),
            Some(1_843_200)
        );
        assert_eq!(
            node.get_property("current-speed").unwrap().get_u32(),
            Some(115_200)
        );
        assert_eq!(node.get_property("reg-shift").unwrap().get_u32(), Some(0));
        assert_eq!(
            node.get_property("reg-io-width").unwrap().get_u32(),
            Some(1)
        );
        assert_eq!(
            reparsed
                .get_by_path("/chosen")
                .unwrap()
                .as_node()
                .get_property("bootargs")
                .unwrap()
                .as_str(),
            Some(
                "root=/dev/vda earlycon=uart8250,mmio,0x9010000,115200 console=ttyS0,115200 -- -n \
                 -l /bin/sh"
            )
        );
    }

    #[test]
    fn generated_pl011_reuses_retained_apb_clock() {
        let mut fdt = console_source_fdt();
        let root = fdt.root_id();
        let clock = fdt.add_node(root, Node::new("apb-pclk"));
        fdt.node_mut(clock)
            .unwrap()
            .set_property(prop_u32("phandle", 7));
        let cfg = console_config(0x0900_0000, 33, alloc::vec![]);
        let dtb = super::create_guest_fdt(&fdt, &["/apb-pclk".into()], &cfg).unwrap();
        let reparsed = Fdt::from_bytes(&dtb).unwrap();
        let clocks = reparsed
            .get_by_path("/pl011@9000000")
            .unwrap()
            .as_node()
            .get_property("clocks")
            .unwrap()
            .get_u32_iter()
            .collect::<alloc::vec::Vec<_>>();

        assert_eq!(clocks, [7, 7]);
        assert!(reparsed.get_by_path_id("/pl011-clock").is_none());
    }

    #[test]
    fn generated_pl011_clock_uses_collision_free_phandle() {
        let mut fdt = console_source_fdt();
        let root = fdt.root_id();
        let used = fdt.add_node(root, Node::new("used-phandle"));
        fdt.node_mut(used)
            .unwrap()
            .set_property(prop_u32("linux,phandle", 1));
        let cfg = console_config(0x0900_0000, 33, alloc::vec![]);
        let dtb = super::create_guest_fdt(&fdt, &[], &cfg).unwrap();
        let reparsed = Fdt::from_bytes(&dtb).unwrap();
        let clock = reparsed.get_by_path("/pl011-clock").unwrap().as_node();

        assert_eq!(
            clock.get_property("compatible").unwrap().as_str(),
            Some("fixed-clock")
        );
        assert_eq!(
            clock.get_property("clock-frequency").unwrap().get_u32(),
            Some(24_000_000)
        );
        assert_eq!(clock.get_property("phandle").unwrap().get_u32(), Some(2));
        assert_eq!(
            reparsed
                .get_by_path("/pl011@9000000")
                .unwrap()
                .as_node()
                .get_property("clocks")
                .unwrap()
                .get_u32_iter()
                .collect::<alloc::vec::Vec<_>>(),
            [2, 2]
        );
    }

    #[test]
    fn generated_console_rejects_invalid_intids_and_subtypes() {
        let fdt = console_source_fdt();
        for cfg in [
            console_config(0x0900_0000, 31, alloc::vec![]),
            console_config(0x0900_0000, 1020, alloc::vec![]),
            console_config(0x0900_0000, 33, alloc::vec![2]),
        ] {
            let error = super::create_guest_fdt(&fdt, &[], &cfg).unwrap_err();
            assert!(matches!(error, crate::AxVmError::InvalidInput { .. }));
        }
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
}
