//! Host-derived guest FDT generation from a finalized machine plan.

mod dependencies;
mod virtual_devices;

use alloc::{
    collections::{BTreeMap, BTreeSet},
    format,
    string::String,
    vec::Vec,
};

use fdt_edit::{Fdt, Node, NodeId, Property};
use fdt_raw::RegInfo;

use self::{
    dependencies::resolve_dependencies,
    virtual_devices::{materialize_virtual_devices, sanitize_virtual_device_templates},
};
use super::{
    DeviceDisposition, HostPlatformSnapshot, MachinePlanError, MachinePlanResult, VmMachinePlan,
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
    sanitize_virtual_device_templates(&mut source, plan)?;
    let selected = selected_paths(plan, &source)?;
    let mut guest = clone_selected_tree(&source, &selected, config)?;
    rebuild_memory(&mut guest, plan)?;
    patch_chosen(&mut guest, config)?;
    materialize_virtual_devices(&mut guest, plan)?;
    guest.boot_cpuid_phys = 0;
    guest.memory_reservations.clear();
    Ok(guest.encode().as_ref().to_vec())
}

fn selected_paths(plan: &VmMachinePlan, source: &Fdt) -> MachinePlanResult<BTreeSet<String>> {
    let mut selected = BTreeSet::from([String::from("/")]);
    for device in plan.host_devices() {
        if matches!(
            device.disposition(),
            DeviceDisposition::Passthrough | DeviceDisposition::Structural
        ) || is_mandatory_fdt_infrastructure(device.compatibles())
        {
            selected.insert(device.id().as_str().into());
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

    let protected = plan
        .host_devices()
        .iter()
        .filter_map(|device| {
            let classification = match device.disposition() {
                DeviceDisposition::HostExclusive => "host-exclusive",
                DeviceDisposition::Denied => "denied",
                DeviceDisposition::Unrepresentable => "unrepresentable",
                DeviceDisposition::VirtualReplacement
                | DeviceDisposition::Passthrough
                | DeviceDisposition::Structural => return None,
            };
            (!selected.contains(device.id().as_str()))
                .then(|| (device.id().as_str().into(), classification))
        })
        .collect::<BTreeMap<_, _>>();
    selected = resolve_dependencies(source, selected, &protected)?;
    add_ancestors(&mut selected);
    Ok(selected)
}

fn is_mandatory_fdt_infrastructure(compatibles: &[String]) -> bool {
    compatibles.iter().any(|compatible| {
        matches!(
            compatible.as_str(),
            "arm,gic-v3" | "arm,gic-v3-its" | "arm,armv8-timer" | "arm,psci-0.2"
        ) || compatible.starts_with("riscv,plic")
            || compatible.starts_with("riscv,cpu-intc")
    })
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
    let unit_id = path
        .strip_prefix("/cpus/cpu@")
        .and_then(|value| value.split('/').next())
        .and_then(|value| usize::from_str_radix(value, 16).ok());
    let reg_id = source
        .view_typed(node_id)
        .and_then(|view| view.regs().first().map(|reg| reg.address as usize));
    unit_id
        .or(reg_id)
        .is_some_and(|id| config.physical_cpu_ids.contains(&id))
}

fn is_host_memory_path(path: &str) -> bool {
    path.starts_with("/memory@")
        || path == "/reserved-memory"
        || path.starts_with("/reserved-memory/")
}

fn rebuild_memory(guest: &mut Fdt, plan: &VmMachinePlan) -> MachinePlanResult<()> {
    let root = guest.root_id();
    for memory in plan.guest_memory() {
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
