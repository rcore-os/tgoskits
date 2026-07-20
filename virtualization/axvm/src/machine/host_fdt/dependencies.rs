//! FDT phandle dependencies shared by snapshot normalization and guest filtering.

use alloc::{
    collections::{BTreeMap, BTreeSet},
    string::{String, ToString},
    vec::Vec,
};

use fdt_edit::{Fdt, Node, NodeId};

use crate::machine::{HostDeviceDependencyKind, MachinePlanError, MachinePlanResult};

type PhandleMap = BTreeMap<u32, PhandleProvider>;

struct PhandleProvider {
    path: String,
    cells: BTreeMap<String, u32>,
}

/// One decoded reference from a node property to a provider node.
pub(crate) struct FdtNodeDependency {
    provider: String,
    property: String,
    kind: HostDeviceDependencyKind,
}

impl FdtNodeDependency {
    pub(crate) fn provider(&self) -> &str {
        &self.provider
    }

    pub(crate) fn property(&self) -> &str {
        &self.property
    }

    pub(crate) const fn kind(&self) -> HostDeviceDependencyKind {
        self.kind
    }
}

/// Provider metadata needed to decode variable-width phandle lists.
pub(crate) struct FdtDependencyIndex {
    providers: PhandleMap,
}

impl FdtDependencyIndex {
    pub(crate) fn new(fdt: &Fdt) -> Self {
        Self {
            providers: phandle_providers(fdt),
        }
    }

    pub(crate) fn dependencies(&self, node: &Node) -> Vec<FdtNodeDependency> {
        node.properties()
            .iter()
            .filter(|property| is_phandle_property(property.name()))
            .flat_map(|property| {
                property_dependencies(property.name(), &property.data, &self.providers)
                    .into_iter()
                    .map(|provider| FdtNodeDependency {
                        provider,
                        property: property.name().to_string(),
                        kind: dependency_kind(property.name()),
                    })
            })
            .collect()
    }
}

pub(super) fn resolve_dependencies(
    fdt: &mut Fdt,
    mut selected: BTreeSet<String>,
    protected: &BTreeMap<String, &'static str>,
) -> MachinePlanResult<BTreeSet<String>> {
    let dependencies = FdtDependencyIndex::new(fdt);
    let paths = fdt
        .iter_node_ids()
        .map(|node| (fdt.path_of(node), node))
        .collect::<BTreeMap<_, _>>();
    let mut pending = selected.iter().cloned().collect::<Vec<_>>();
    while let Some(path) = pending.pop() {
        let Some(node_id) = paths.get(&path).copied() else {
            continue;
        };
        let Some(node) = fdt.node(node_id) else {
            continue;
        };
        let node_dependencies = dependencies.dependencies(node);
        for property in dependency_properties(node_dependencies) {
            let unavailable = property.dependencies.iter().find_map(|dependency| {
                protected
                    .get(dependency.provider())
                    .map(|classification| (dependency, *classification))
            });
            if let Some((dependency, classification)) = unavailable {
                if property.name == "interrupts-extended"
                    && is_cpu_context_table(&property.dependencies)
                {
                    for provider in retain_assigned_cpu_contexts(
                        fdt,
                        node_id,
                        &path,
                        protected,
                        &dependencies.providers,
                    )? {
                        if selected.insert(provider.clone()) {
                            pending.push(provider);
                        }
                    }
                    continue;
                }
                if property.kind == HostDeviceDependencyKind::Optional {
                    remove_optional_property(fdt, node_id, &property.name, &path)?;
                    continue;
                }
                return Err(MachinePlanError::InvalidFirmware {
                    detail: alloc::format!(
                        "FDT node '{path}' depends on {classification} node '{}' through required \
                         property '{}'",
                        dependency.provider(),
                        property.name,
                    ),
                });
            }
            for dependency in property.dependencies {
                if selected.insert(dependency.provider().into()) {
                    pending.push(dependency.provider().into());
                }
            }
        }
    }
    Ok(selected)
}

fn is_cpu_context_table(dependencies: &[FdtNodeDependency]) -> bool {
    !dependencies.is_empty()
        && dependencies.iter().all(|dependency| {
            dependency.provider().starts_with("/cpus/cpu@")
                && dependency.provider().ends_with("/interrupt-controller")
        })
}

fn retain_assigned_cpu_contexts(
    fdt: &mut Fdt,
    node_id: NodeId,
    path: &str,
    protected: &BTreeMap<String, &'static str>,
    providers: &PhandleMap,
) -> MachinePlanResult<Vec<String>> {
    let bytes = fdt
        .node(node_id)
        .and_then(|node| node.get_property("interrupts-extended"))
        .ok_or_else(|| MachinePlanError::InvalidFirmware {
            detail: alloc::format!("FDT node '{path}' has no interrupts-extended property"),
        })?
        .data
        .clone();
    if bytes.is_empty() || !bytes.len().is_multiple_of(4) {
        return Err(MachinePlanError::InvalidFirmware {
            detail: alloc::format!(
                "FDT node '{path}' has a malformed interrupts-extended property"
            ),
        });
    }
    let cells = bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|cell| u32::from_be_bytes(*cell))
        .collect::<Vec<_>>();
    let mut retained_cells = Vec::new();
    let mut retained_providers = Vec::new();
    let mut index = 0;
    while index < cells.len() {
        let provider =
            providers
                .get(&cells[index])
                .ok_or_else(|| MachinePlanError::InvalidFirmware {
                    detail: alloc::format!(
                        "FDT node '{path}' refers to unknown interrupt provider phandle {:#x}",
                        cells[index],
                    ),
                })?;
        let width = 1 + argument_cell_count("interrupts-extended", &provider.cells);
        let end = index
            .checked_add(width)
            .filter(|end| *end <= cells.len())
            .ok_or_else(|| MachinePlanError::InvalidFirmware {
                detail: alloc::format!(
                    "FDT node '{path}' has a truncated interrupt context for provider '{}'",
                    provider.path,
                ),
            })?;
        if !protected.contains_key(&provider.path) {
            retained_cells.extend_from_slice(&cells[index..end]);
            retained_providers.push(provider.path.clone());
        }
        index = end;
    }
    if retained_cells.is_empty() {
        return Err(MachinePlanError::InvalidFirmware {
            detail: alloc::format!(
                "FDT node '{path}' has no interrupt context for an assigned CPU"
            ),
        });
    }
    let mut property = fdt_edit::Property::new("interrupts-extended", Vec::new());
    property.set_u32_ls(&retained_cells);
    fdt.node_mut(node_id)
        .ok_or_else(|| MachinePlanError::InvalidFirmware {
            detail: alloc::format!("FDT node '{path}' interrupt contexts cannot be updated"),
        })?
        .set_property(property);
    Ok(retained_providers)
}

struct PropertyDependencies {
    name: String,
    kind: HostDeviceDependencyKind,
    dependencies: Vec<FdtNodeDependency>,
}

fn dependency_properties(dependencies: Vec<FdtNodeDependency>) -> Vec<PropertyDependencies> {
    let mut properties = Vec::<PropertyDependencies>::new();
    for dependency in dependencies {
        if let Some(property) = properties
            .iter_mut()
            .find(|property| property.name == dependency.property())
        {
            property.dependencies.push(dependency);
        } else {
            properties.push(PropertyDependencies {
                name: dependency.property.clone(),
                kind: dependency.kind,
                dependencies: alloc::vec![dependency],
            });
        }
    }
    properties
}

fn remove_optional_property(
    fdt: &mut Fdt,
    node_id: NodeId,
    property: &str,
    path: &str,
) -> MachinePlanResult<()> {
    let node = fdt
        .node_mut(node_id)
        .ok_or_else(|| MachinePlanError::InvalidFirmware {
            detail: alloc::format!("FDT node '{path}' disappeared while removing '{property}'"),
        })?;
    node.remove_property(property);
    if let Some(companion) = companion_property(property) {
        node.remove_property(companion);
    }
    Ok(())
}

fn companion_property(property: &str) -> Option<&'static str> {
    match property {
        "dmas" => Some("dma-names"),
        "interconnects" => Some("interconnect-names"),
        "memory-region" => Some("memory-region-names"),
        "nvmem-cells" => Some("nvmem-cell-names"),
        name if name.starts_with("pinctrl-") => Some("pinctrl-names"),
        _ => None,
    }
}

fn phandle_providers(fdt: &Fdt) -> PhandleMap {
    let mut providers = BTreeMap::new();
    for node_id in fdt.iter_node_ids() {
        let Some(node) = fdt.node(node_id) else {
            continue;
        };
        let mut phandle = None;
        let mut cells = BTreeMap::new();
        for property in node.properties() {
            match property.name() {
                "phandle" | "linux,phandle" => phandle = property.get_u32(),
                name if name.starts_with('#') && name.ends_with("-cells") => {
                    if let Some(value) = property.get_u32() {
                        cells.insert(name.into(), value);
                    }
                }
                _ => {}
            }
        }
        if let Some(phandle) = phandle {
            providers.insert(
                phandle,
                PhandleProvider {
                    path: fdt.path_of(node_id),
                    cells,
                },
            );
        }
    }
    providers
}

fn property_dependencies(name: &str, bytes: &[u8], providers: &PhandleMap) -> Vec<String> {
    if bytes.is_empty() || !bytes.len().is_multiple_of(4) {
        return Vec::new();
    }
    let cells = bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|cell| u32::from_be_bytes(*cell))
        .collect::<Vec<_>>();
    let mut dependencies = Vec::new();
    let mut index = 0;
    while index < cells.len() {
        let Some(provider) = providers.get(&cells[index]) else {
            index += 1;
            continue;
        };
        dependencies.push(provider.path.clone());
        let arguments = argument_cell_count(name, &provider.cells);
        index = index.saturating_add(arguments + 1);
    }
    dependencies
}

fn argument_cell_count(name: &str, cells: &BTreeMap<String, u32>) -> usize {
    let cell_property = match name {
        "interrupt-parent"
        | "phy-handle"
        | "msi-parent"
        | "memory-region"
        | "shmem"
        | "operating-points-v2"
        | "cpu-idle-states"
        | "syscon"
        | "regmap"
        | "nvmem-cells"
        | "pinctrl-0"
        | "pinctrl-1"
        | "pinctrl-2"
        | "pinctrl-3"
        | "pinctrl-4"
        | "remote-endpoint" => return 0,
        "clocks" | "assigned-clocks" | "assigned-clock-parents" => "#clock-cells",
        "resets" => "#reset-cells",
        "power-domains" => "#power-domain-cells",
        "performance-domains" => "#performance-domain-cells",
        "phys" => "#phy-cells",
        "interrupts-extended" => "#interrupt-cells",
        "gpio" | "gpios" => "#gpio-cells",
        name if name.ends_with("-gpios") || name.ends_with("-gpio") => "#gpio-cells",
        "dmas" => "#dma-cells",
        "thermal-sensors" => "#thermal-sensor-cells",
        "sound-dai" => "#sound-dai-cells",
        "mboxes" => "#mbox-cells",
        "pwms" => "#pwm-cells",
        "iommus" => "#iommu-cells",
        "interconnects" => "#interconnect-cells",
        "cooling-device" => "#cooling-cells",
        _ => return 0,
    };
    cells.get(cell_property).copied().unwrap_or(0) as usize
}

fn dependency_kind(name: &str) -> HostDeviceDependencyKind {
    if matches!(
        name,
        "dmas"
            | "thermal-sensors"
            | "interconnects"
            | "memory-region"
            | "nvmem-cells"
            | "remote-endpoint"
            | "cooling-device"
            | "gpio"
            | "gpios"
    ) || name.starts_with("pinctrl-")
        || name.ends_with("-supply")
        || name.ends_with("-gpios")
        || name.ends_with("-gpio")
    {
        HostDeviceDependencyKind::Optional
    } else {
        HostDeviceDependencyKind::Required
    }
}

fn is_phandle_property(name: &str) -> bool {
    matches!(
        name,
        "clocks"
            | "cpu-idle-states"
            | "operating-points-v2"
            | "performance-domains"
            | "power-domains"
            | "phys"
            | "resets"
            | "dmas"
            | "thermal-sensors"
            | "mboxes"
            | "assigned-clocks"
            | "assigned-clock-parents"
            | "interrupt-parent"
            | "interrupts-extended"
            | "phy-handle"
            | "msi-parent"
            | "memory-region"
            | "shmem"
            | "remote-endpoint"
            | "cooling-device"
            | "syscon"
            | "regmap"
            | "iommus"
            | "interconnects"
            | "nvmem-cells"
            | "sound-dai"
            | "pinctrl-0"
            | "pinctrl-1"
            | "pinctrl-2"
            | "pinctrl-3"
            | "pinctrl-4"
            | "gpio"
            | "gpios"
            | "pwms"
    ) || name.ends_with("-supply")
        || name.ends_with("-gpios")
        || name.ends_with("-gpio")
}
