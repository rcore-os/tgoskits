//! Host-derived guest FDT generation from a finalized machine plan.

pub(super) mod dependencies;
mod virtual_devices;

use alloc::{
    collections::{BTreeMap, BTreeSet},
    format,
    string::{String, ToString},
    vec::Vec,
};

use fdt_edit::{Fdt, Node, NodeId, Property};
use fdt_raw::RegInfo;

use self::{
    dependencies::resolve_dependencies,
    virtual_devices::{materialize_virtual_devices, sanitize_virtual_device_templates},
};
use super::{
    DeviceDisposition, HostFirmwareActivation, HostPlatformSnapshot, MachinePlanError,
    MachinePlanResult, VmMachinePlan,
    fdt::{is_direct_cpu_node, is_host_managed_cpu_property},
    is_planned_guest_firmware_infrastructure,
};

/// Guest-specific data used while filtering a host FDT snapshot.
#[derive(Clone, Debug)]
pub struct HostFdtConfig {
    physical_cpu_ids: BTreeSet<usize>,
    bootargs: Option<String>,
}

impl HostFdtConfig {
    /// Creates a host-derived FDT configuration for the assigned physical CPUs.
    pub fn new(physical_cpu_ids: impl IntoIterator<Item = usize>) -> Self {
        Self {
            physical_cpu_ids: physical_cpu_ids.into_iter().collect(),
            bootargs: None,
        }
    }

    /// Replaces the host command line with a guest-specific command line.
    pub fn with_bootargs(mut self, bootargs: impl Into<String>) -> Self {
        self.bootargs = Some(bootargs.into());
        self
    }
}

/// Filters a captured host FDT according to one immutable machine plan.
///
/// Passthrough devices, structural nodes, controller/timer infrastructure, and
/// recursively referenced phandle providers are retained. Denied devices and
/// host RAM are removed, then guest RAM and virtual-device fallback nodes are
/// rebuilt from resolved plan resources.
pub fn generate_host_fdt(
    plan: &VmMachinePlan,
    snapshot: &HostPlatformSnapshot,
    config: &HostFdtConfig,
) -> MachinePlanResult<Vec<u8>> {
    if plan.snapshot_generation() != snapshot.generation() {
        return Err(MachinePlanError::SnapshotGenerationChanged {
            planned: plan.snapshot_generation(),
            current: snapshot.generation(),
        });
    }
    let bytes = snapshot
        .source_fdt()
        .ok_or_else(|| MachinePlanError::InvalidFirmware {
            detail: "host platform snapshot has no source FDT".into(),
        })?;
    let mut source = Fdt::from_bytes(bytes).map_err(|error| MachinePlanError::InvalidFirmware {
        detail: format!("failed to parse captured host FDT: {error:?}"),
    })?;
    sanitize_host_managed_cpu_properties(&mut source)?;
    sanitize_virtual_device_templates(&mut source, plan)?;
    let selected = selected_paths(plan, &mut source, config)?;
    let mut guest = clone_selected_tree(&source, &selected, config)?;
    mark_passthrough_devices_available(&mut guest, plan)?;
    sanitize_path_tables(&mut guest)?;
    rebuild_memory(&mut guest, plan)?;
    patch_chosen(&mut guest, config)?;
    normalize_rockchip_fiq_console(&source, &mut guest, plan)?;
    materialize_virtual_devices(&mut guest, plan)?;
    sanitize_virtual_console_bootargs(&mut guest, plan)?;
    guest.boot_cpuid_phys = 0;
    guest.memory_reservations.clear();
    Ok(guest.encode().as_ref().to_vec())
}

fn selected_paths(
    plan: &VmMachinePlan,
    source: &mut Fdt,
    config: &HostFdtConfig,
) -> MachinePlanResult<BTreeSet<String>> {
    let mut selected = BTreeSet::from([String::from("/")]);
    for device in plan.host_devices() {
        let path = device.id().as_str();
        if source
            .get_by_path_id(path)
            .is_some_and(|node| !selected_cpu(source, node, path, config))
        {
            continue;
        }
        if matches!(
            device.disposition(),
            DeviceDisposition::Passthrough | DeviceDisposition::Structural
        ) || selected_guest_firmware_infrastructure(plan, device.compatibles())
        {
            selected.insert(path.into());
        }
    }
    for device in plan.virtual_devices() {
        if let Some(template) = device.host_template() {
            selected.insert(template.as_str().into());
        }
    }
    for conventional in ["/aliases", "/chosen", "/cpus"] {
        if source.get_by_path_id(conventional).is_some() {
            selected.insert(conventional.into());
        }
    }

    let mut protected = plan
        .host_devices()
        .iter()
        .filter_map(|device| {
            let classification = match device.disposition() {
                DeviceDisposition::HostExclusive => "host-exclusive",
                DeviceDisposition::Denied => "denied",
                DeviceDisposition::Inactive => "inactive",
                DeviceDisposition::Unrepresentable => "unrepresentable",
                DeviceDisposition::VirtualReplacement
                | DeviceDisposition::Passthrough
                | DeviceDisposition::Structural => return None,
            };
            (!selected.contains(device.id().as_str()))
                .then(|| (device.id().as_str().into(), classification))
        })
        .collect::<BTreeMap<_, _>>();
    for node_id in source.iter_node_ids() {
        let path = source.path_of(node_id);
        if !selected_cpu(source, node_id, &path, config) {
            protected.insert(path, "unassigned CPU");
        }
    }
    selected = resolve_dependencies(source, selected, &protected)?;
    add_ancestors(&mut selected);
    Ok(selected)
}

fn selected_guest_firmware_infrastructure(plan: &VmMachinePlan, compatibles: &[String]) -> bool {
    is_planned_guest_firmware_infrastructure(plan.interrupt_controller(), compatibles)
}

fn add_ancestors(paths: &mut BTreeSet<String>) {
    let selected = paths.iter().cloned().collect::<Vec<_>>();
    for path in selected {
        let mut cursor = path.as_str();
        while let Some((parent, _)) = cursor.rsplit_once('/') {
            let parent = if parent.is_empty() { "/" } else { parent };
            paths.insert(parent.into());
            if parent == "/" {
                break;
            }
            cursor = parent;
        }
    }
}

fn clone_selected_tree(
    source: &Fdt,
    selected: &BTreeSet<String>,
    config: &HostFdtConfig,
) -> MachinePlanResult<Fdt> {
    let mut guest = Fdt::new();
    let source_root =
        source
            .node(source.root_id())
            .ok_or_else(|| MachinePlanError::InvalidFirmware {
                detail: "captured host FDT has no root node".into(),
            })?;
    let guest_root = guest.root_id();
    let guest_root_node =
        guest
            .node_mut(guest_root)
            .ok_or_else(|| MachinePlanError::InvalidFirmware {
                detail: "new guest FDT has no mutable root node".into(),
            })?;
    copy_properties(source_root, guest_root_node);
    copy_children(
        source,
        source.root_id(),
        &mut guest,
        guest_root,
        selected,
        config,
    )?;
    Ok(guest)
}

fn mark_passthrough_devices_available(
    guest: &mut Fdt,
    plan: &VmMachinePlan,
) -> MachinePlanResult<()> {
    for device in plan.host_devices().iter().filter(|device| {
        device.disposition() == DeviceDisposition::Passthrough
            && device.firmware_activation() == HostFirmwareActivation::Enable
    }) {
        let path = device.id().as_str();
        let Some(node_id) = guest.get_by_path_id(path) else {
            continue;
        };
        let disabled = guest
            .node(node_id)
            .ok_or_else(|| MachinePlanError::InvalidFirmware {
                detail: format!("guest FDT passthrough device '{path}' disappeared"),
            })?
            .get_property("status")
            .and_then(Property::as_str)
            == Some("disabled");
        if disabled {
            guest
                .node_mut(node_id)
                .ok_or_else(|| MachinePlanError::InvalidFirmware {
                    detail: format!("guest FDT passthrough device '{path}' cannot be updated"),
                })?
                .set_property(string_property("status", "okay"));
        }
    }
    Ok(())
}

fn copy_children(
    source: &Fdt,
    source_parent: NodeId,
    guest: &mut Fdt,
    guest_parent: NodeId,
    selected: &BTreeSet<String>,
    config: &HostFdtConfig,
) -> MachinePlanResult<()> {
    let children = source
        .node(source_parent)
        .ok_or_else(|| MachinePlanError::InvalidFirmware {
            detail: "captured host FDT contains an invalid node reference".into(),
        })?
        .children()
        .to_vec();
    for source_id in children {
        let path = source.path_of(source_id);
        if !selected.contains(&path)
            || is_host_memory_path(&path)
            || !selected_cpu(source, source_id, &path, config)
        {
            continue;
        }
        let source_node =
            source
                .node(source_id)
                .ok_or_else(|| MachinePlanError::InvalidFirmware {
                    detail: format!("captured host FDT node '{path}' disappeared"),
                })?;
        let guest_id = guest.add_node(guest_parent, Node::new(source_node.name()));
        let guest_node =
            guest
                .node_mut(guest_id)
                .ok_or_else(|| MachinePlanError::InvalidFirmware {
                    detail: format!("new guest FDT node '{path}' cannot be updated"),
                })?;
        copy_properties(source_node, guest_node);
        copy_children(source, source_id, guest, guest_id, selected, config)?;
    }
    Ok(())
}

fn selected_cpu(source: &Fdt, node_id: NodeId, path: &str, config: &HostFdtConfig) -> bool {
    if !path.starts_with("/cpus/cpu@") {
        return true;
    }
    let unit_name = path
        .strip_prefix("/cpus/cpu@")
        .and_then(|value| value.split('/').next());
    let unit_id = unit_name.and_then(|value| usize::from_str_radix(value, 16).ok());
    let cpu_node = unit_name
        .and_then(|unit| source.get_by_path_id(&format!("/cpus/cpu@{unit}")))
        .unwrap_or(node_id);
    let reg_id = source
        .view_typed(cpu_node)
        .and_then(|view| view.regs().first().map(|reg| reg.address as usize));
    reg_id
        .or(unit_id)
        .is_some_and(|id| config.physical_cpu_ids.contains(&id))
}

fn sanitize_host_managed_cpu_properties(source: &mut Fdt) -> MachinePlanResult<()> {
    let cpu_nodes = source
        .iter_node_ids()
        .filter_map(|node_id| {
            let path = source.path_of(node_id);
            is_direct_cpu_node(&path).then_some((node_id, path))
        })
        .collect::<Vec<_>>();
    for (node_id, path) in cpu_nodes {
        let properties = source
            .node(node_id)
            .ok_or_else(|| MachinePlanError::InvalidFirmware {
                detail: format!("captured host CPU node '{path}' disappeared"),
            })?
            .properties()
            .iter()
            .map(|property| property.name().to_string())
            .filter(|property| is_host_managed_cpu_property(&path, property))
            .collect::<Vec<_>>();
        let node = source
            .node_mut(node_id)
            .ok_or_else(|| MachinePlanError::InvalidFirmware {
                detail: format!("captured host CPU node '{path}' cannot be sanitized"),
            })?;
        for property in properties {
            node.remove_property(&property);
        }
    }
    Ok(())
}

fn sanitize_path_tables(guest: &mut Fdt) -> MachinePlanResult<()> {
    for table_path in ["/aliases", "/__symbols__"] {
        let Some(table_id) = guest.get_by_path_id(table_path) else {
            continue;
        };
        let references = guest
            .node(table_id)
            .ok_or_else(|| MachinePlanError::InvalidFirmware {
                detail: format!("guest FDT path table '{table_path}' disappeared"),
            })?
            .properties()
            .iter()
            .filter_map(|property| {
                property.as_str().map(|target| {
                    (
                        property.name().to_string(),
                        target.split(':').next().unwrap_or(target).to_string(),
                    )
                })
            })
            .collect::<Vec<_>>();
        let stale = references
            .into_iter()
            .filter_map(|(property, target)| {
                (target.starts_with('/') && guest.get_by_path_id(&target).is_none())
                    .then_some(property)
            })
            .collect::<Vec<_>>();
        let table = guest
            .node_mut(table_id)
            .ok_or_else(|| MachinePlanError::InvalidFirmware {
                detail: format!("guest FDT path table '{table_path}' cannot be updated"),
            })?;
        for property in stale {
            table.remove_property(&property);
        }
    }
    sanitize_stdout_path(guest)
}

fn sanitize_stdout_path(guest: &mut Fdt) -> MachinePlanResult<()> {
    let Some(chosen_id) = guest.get_by_path_id("/chosen") else {
        return Ok(());
    };
    let Some(target) = guest
        .node(chosen_id)
        .and_then(|node| node.get_property("stdout-path"))
        .and_then(Property::as_str)
        .map(|target| target.split(':').next().unwrap_or(target).to_string())
    else {
        return Ok(());
    };
    let target_exists = if target.starts_with('/') {
        guest.get_by_path_id(&target).is_some()
    } else {
        guest
            .get_by_path("/aliases")
            .is_some_and(|aliases| aliases.as_node().get_property(&target).is_some())
    };
    if !target_exists {
        guest
            .node_mut(chosen_id)
            .ok_or_else(|| MachinePlanError::InvalidFirmware {
                detail: "guest /chosen node cannot be updated".into(),
            })?
            .remove_property("stdout-path");
    }
    Ok(())
}

fn is_host_memory_path(path: &str) -> bool {
    path.starts_with("/memory@")
        || path == "/reserved-memory"
        || path.starts_with("/reserved-memory/")
}

fn rebuild_memory(guest: &mut Fdt, plan: &VmMachinePlan) -> MachinePlanResult<()> {
    let root = guest.root_id();
    for memory in plan.fixed_guest_memory() {
        let node = guest.add_node(root, Node::new(&format!("memory@{:x}", memory.base())));
        guest
            .node_mut(node)
            .ok_or_else(|| MachinePlanError::InvalidFirmware {
                detail: format!(
                    "new guest memory node at {:#x} cannot be updated",
                    memory.base()
                ),
            })?
            .set_property(string_property("device_type", "memory"));
        guest
            .view_typed_mut(node)
            .ok_or_else(|| MachinePlanError::InvalidFirmware {
                detail: "new guest memory node cannot be represented".into(),
            })?
            .set_regs(&[RegInfo::new(memory.base(), Some(memory.size()))]);
    }
    Ok(())
}

fn patch_chosen(guest: &mut Fdt, config: &HostFdtConfig) -> MachinePlanResult<()> {
    let chosen = match guest.get_by_path_id("/chosen") {
        Some(chosen) => chosen,
        None => guest.add_node(guest.root_id(), Node::new("chosen")),
    };
    let chosen = guest
        .node_mut(chosen)
        .ok_or_else(|| MachinePlanError::InvalidFirmware {
            detail: "guest /chosen node cannot be updated".into(),
        })?;
    chosen.remove_property("linux,initrd-start");
    chosen.remove_property("linux,initrd-end");
    if let Some(bootargs) = config.bootargs.as_deref() {
        chosen.set_property(string_property("bootargs", bootargs));
    }
    Ok(())
}

fn sanitize_virtual_console_bootargs(
    guest: &mut Fdt,
    plan: &VmMachinePlan,
) -> MachinePlanResult<()> {
    let Some(model) = plan
        .virtual_devices()
        .iter()
        .map(|device| device.model_id().as_str())
        .find(|model| matches!(*model, "arm-pl011" | "ns16550a" | "snps-dw-apb-uart"))
    else {
        return Ok(());
    };
    let Some(chosen_id) = guest.get_by_path_id("/chosen") else {
        return Ok(());
    };
    let Some(bootargs) = guest
        .node(chosen_id)
        .and_then(|chosen| chosen.get_property("bootargs"))
        .and_then(Property::as_str)
    else {
        return Ok(());
    };
    let bootargs = virtual_console_bootargs(bootargs, model);
    guest
        .node_mut(chosen_id)
        .ok_or_else(|| MachinePlanError::InvalidFirmware {
            detail: "guest /chosen node cannot receive virtual-console boot arguments".into(),
        })?
        .set_property(string_property("bootargs", &bootargs));
    Ok(())
}

fn virtual_console_bootargs(bootargs: &str, model: &str) -> String {
    let mut arguments = Vec::new();
    let mut early_console = false;
    for argument in bootargs.split_ascii_whitespace() {
        if argument == "keep_bootcon" || argument.starts_with("earlyprintk") {
            continue;
        }
        if argument == "earlycon" || argument.starts_with("earlycon=") {
            if !early_console {
                arguments.push(String::from("earlycon"));
                early_console = true;
            }
            continue;
        }
        if let Some(console) = argument.strip_prefix("console=") {
            let serial_console = console.starts_with("ttyAMA")
                || console.starts_with("ttyS")
                || console.starts_with("ttyFIQ");
            let matches_model = match model {
                "arm-pl011" => console.starts_with("ttyAMA"),
                "ns16550a" | "snps-dw-apb-uart" => console.starts_with("ttyS"),
                _ => false,
            };
            if serial_console && !matches_model {
                continue;
            }
        }
        arguments.push(argument.into());
    }
    arguments.join(" ")
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RockchipFiqConsole {
    alias: String,
    baud: u32,
}

fn normalize_rockchip_fiq_console(
    source: &Fdt,
    guest: &mut Fdt,
    plan: &VmMachinePlan,
) -> MachinePlanResult<()> {
    let Some(console_path) = plan.host_console().map(|console| console.as_str()) else {
        return Ok(());
    };
    let Some(console) = rockchip_fiq_console(source, console_path)? else {
        return Ok(());
    };
    let Some(uart) = guest.get_by_path_id(console_path) else {
        // The host console remains available as a mediated backend, but its
        // physical UART is deliberately absent from the guest device tree.
        return Ok(());
    };
    guest
        .node_mut(uart)
        .ok_or_else(|| MachinePlanError::InvalidFirmware {
            detail: format!("Rockchip console UART '{console_path}' cannot be updated"),
        })?
        .set_property(string_property("status", "okay"));

    let chosen =
        guest
            .get_by_path_id("/chosen")
            .ok_or_else(|| MachinePlanError::InvalidFirmware {
                detail: "guest /chosen node is missing during Rockchip console normalization"
                    .into(),
            })?;
    let chosen = guest
        .node_mut(chosen)
        .ok_or_else(|| MachinePlanError::InvalidFirmware {
            detail: "guest /chosen node cannot be updated during Rockchip console normalization"
                .into(),
        })?;
    if let Some(bootargs) = chosen.get_property("bootargs").and_then(Property::as_str) {
        chosen.set_property(string_property(
            "bootargs",
            &rewrite_rockchip_fiq_console(bootargs, &console),
        ));
    }
    chosen.set_property(string_property(
        "stdout-path",
        &format!("{}:{}n8", console.alias, console.baud),
    ));
    Ok(())
}

fn rockchip_fiq_console(
    source: &Fdt,
    console_path: &str,
) -> MachinePlanResult<Option<RockchipFiqConsole>> {
    let Some(aliases) = source.get_by_path("/aliases") else {
        return Ok(None);
    };
    let aliases = aliases.as_node();
    let mut matched = None;
    for node_id in source.iter_node_ids() {
        let Some(node) = source.node(node_id) else {
            continue;
        };
        if node.get_property("status").and_then(Property::as_str) == Some("disabled")
            || !node
                .compatibles()
                .any(|compatible| compatible == "rockchip,fiq-debugger")
        {
            continue;
        }
        let serial_id = node
            .get_property("rockchip,serial-id")
            .and_then(Property::get_u32)
            .ok_or_else(|| MachinePlanError::InvalidFirmware {
                detail: format!(
                    "Rockchip FIQ debugger '{}' has no serial-id",
                    source.path_of(node_id)
                ),
            })?;
        let alias = format!("serial{serial_id}");
        let alias_path = aliases
            .get_property(&alias)
            .and_then(Property::as_str)
            .ok_or_else(|| MachinePlanError::InvalidFirmware {
                detail: format!(
                    "Rockchip FIQ debugger '{}' refers to missing alias '{alias}'",
                    source.path_of(node_id)
                ),
            })?;
        if alias_path != console_path {
            continue;
        }
        let baud = node
            .get_property("rockchip,baudrate")
            .and_then(Property::get_u32)
            .filter(|baud| *baud != 0)
            .ok_or_else(|| MachinePlanError::InvalidFirmware {
                detail: format!(
                    "Rockchip FIQ debugger '{}' has no valid baud rate",
                    source.path_of(node_id)
                ),
            })?;
        let console = RockchipFiqConsole { alias, baud };
        if matched.replace(console).is_some() {
            return Err(MachinePlanError::InvalidFirmware {
                detail: format!(
                    "multiple Rockchip FIQ debuggers refer to guest console '{console_path}'"
                ),
            });
        }
    }
    Ok(matched)
}

fn rewrite_rockchip_fiq_console(bootargs: &str, console: &RockchipFiqConsole) -> String {
    let serial_id = console.alias.trim_start_matches("serial");
    bootargs
        .split_ascii_whitespace()
        .map(|argument| {
            if argument
                .strip_prefix("console=")
                .is_some_and(|value| value.starts_with("ttyFIQ"))
            {
                format!("console=ttyS{serial_id},{}", console.baud)
            } else {
                argument.into()
            }
        })
        .collect::<Vec<String>>()
        .join(" ")
}

fn copy_properties(source: &Node, destination: &mut Node) {
    for property in source.properties() {
        destination.set_property(property.clone());
    }
}

fn string_property(name: &str, value: &str) -> Property {
    let mut property = Property::new(name, Vec::new());
    property.set_string(value);
    property
}
