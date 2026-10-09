use std::{collections::BTreeSet, format, string::String, vec::Vec};

use fdt_edit::{Fdt, Node, NodeId, Property};
use fdt_raw::{Header, RegInfo};

use crate::{AxVmResult, ax_err_type};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct GuestMemorySpec {
    pub(crate) base: u64,
    pub(crate) size: u64,
}

impl GuestMemorySpec {
    pub(crate) const fn new(base: u64, size: u64) -> Self {
        Self { base, size }
    }
}

pub(crate) struct FdtTree {
    fdt: Fdt,
}

impl FdtTree {
    pub(crate) fn new() -> Self {
        Self { fdt: Fdt::new() }
    }

    pub(crate) fn from_fdt(fdt: Fdt) -> Self {
        Self { fdt }
    }

    pub(crate) fn from_bytes(bytes: &[u8]) -> AxVmResult<Self> {
        let fdt = Fdt::from_bytes(bytes)
            .map_err(|err| ax_err_type!(InvalidData, format!("Failed to parse FDT: {err:#?}")))?;
        Ok(Self::from_fdt(fdt))
    }

    pub(crate) fn inner(&self) -> &Fdt {
        &self.fdt
    }

    pub(crate) fn inner_mut(&mut self) -> &mut Fdt {
        &mut self.fdt
    }

    pub(crate) fn finish(mut self) -> Vec<u8> {
        self.normalize_guest_header();
        self.fdt.encode().as_ref().to_vec()
    }

    fn normalize_guest_header(&mut self) {
        self.fdt.boot_cpuid_phys = 0;
        self.fdt.memory_reservations.clear();
    }

    pub(crate) fn node_paths(&self) -> Vec<(NodeId, String)> {
        self.fdt
            .iter_node_ids()
            .map(|id| (id, self.fdt.path_of(id)))
            .collect()
    }

    pub(crate) fn ensure_path(&mut self, path: &str) -> AxVmResult<NodeId> {
        if let Some(id) = self.fdt.get_by_path_id(path) {
            return Ok(id);
        }

        let normalized = path.trim_matches('/');
        let mut parent = self.fdt.root_id();
        let mut current_path = String::new();

        for part in normalized.split('/').filter(|part| !part.is_empty()) {
            current_path.push('/');
            current_path.push_str(part);
            if let Some(id) = self.fdt.get_by_path_id(&current_path) {
                parent = id;
                continue;
            }
            parent = self.fdt.add_node(parent, Node::new(part));
        }

        Ok(parent)
    }

    pub(crate) fn set_property(&mut self, node_id: NodeId, prop: Property) -> AxVmResult {
        let node = self
            .fdt
            .node_mut(node_id)
            .ok_or_else(|| ax_err_type!(InvalidData, "FDT node id is invalid"))?;
        node.set_property(prop);
        Ok(())
    }

    pub(crate) fn add_node(&mut self, parent: NodeId, node: Node) -> NodeId {
        self.fdt.add_node(parent, node)
    }

    pub(crate) fn node_phandle(&self, node_id: NodeId) -> AxVmResult<Option<u32>> {
        let node = self
            .fdt
            .node(node_id)
            .ok_or_else(|| ax_err_type!(InvalidData, "FDT node id is invalid"))?;
        checked_node_phandle(node)
    }

    pub(crate) fn allocate_phandle(&self) -> AxVmResult<u32> {
        first_available_phandle(self.used_phandles()?)
    }

    pub(crate) fn interrupt_cells(&self, phandle: u32) -> AxVmResult<usize> {
        let mut provider = None;
        for node_id in self.fdt.iter_node_ids() {
            if self.node_phandle(node_id)? == Some(phandle) {
                provider = self.fdt.node(node_id);
                break;
            }
        }
        let provider = provider.ok_or_else(|| {
            ax_err_type!(
                InvalidData,
                std::format!("FDT interrupt provider {phandle:#x} is missing")
            )
        })?;
        let cells = provider
            .get_property("#interrupt-cells")
            .and_then(Property::get_u32)
            .ok_or_else(|| {
                ax_err_type!(
                    InvalidData,
                    std::format!("FDT interrupt provider {phandle:#x} has no #interrupt-cells")
                )
            })?;
        usize::try_from(cells).map_err(|_| {
            ax_err_type!(
                InvalidData,
                std::format!("FDT interrupt provider {phandle:#x} cell count does not fit usize")
            )
        })
    }

    fn used_phandles(&self) -> AxVmResult<BTreeSet<u32>> {
        Ok(super::references::phandle_index(&self.fdt)?
            .into_keys()
            .collect())
    }

    pub(crate) fn validate_phandles(&self) -> AxVmResult {
        super::references::phandle_index(&self.fdt).map(|_| ())
    }

    /// Chooses an identity before replacing a firmware node. Preserve the
    /// guest identity so existing references remain valid; host identities
    /// are only hints and must not displace unrelated guest nodes.
    pub(crate) fn replacement_phandle(
        &self,
        path: &str,
        preferred: Option<u32>,
    ) -> AxVmResult<Option<u32>> {
        self.validate_phandles()?;
        if let Some(node_id) = self.fdt.get_by_path_id(path)
            && let Some(phandle) = self.node_phandle(node_id)?
        {
            return Ok(Some(phandle));
        }
        let Some(preferred) = preferred else {
            return Ok(None);
        };
        if preferred == 0 || preferred == u32::MAX {
            return Err(ax_err_type!(
                InvalidData,
                std::format!("replacement phandle {preferred:#x} is reserved")
            ));
        }
        // Inspect live properties: nodes may have been added or renumbered
        // since this FDT was parsed.
        let used = self.used_phandles()?;
        if !used.contains(&preferred) {
            return Ok(Some(preferred));
        }
        first_available_phandle(used).map(Some)
    }

    pub(crate) fn rebuild_memory_nodes(&mut self, regions: &[GuestMemorySpec]) -> AxVmResult {
        let memory_paths = self
            .node_paths()
            .into_iter()
            .filter_map(|(id, path)| {
                let name = self.fdt.node(id)?.name();
                (name.starts_with("memory") && path != "/").then_some(path)
            })
            .collect::<Vec<_>>();

        self.remove_paths_deepest_first(memory_paths);

        let root = self.fdt.root_id();
        for region in regions {
            if region.size == 0 {
                continue;
            }
            let node_id = self
                .fdt
                .add_node(root, Node::new(&format!("memory@{:x}", region.base)));
            self.set_property(node_id, prop_string("device_type", "memory"))?;
            self.fdt
                .view_typed_mut(node_id)
                .ok_or_else(|| ax_err_type!(InvalidData, "new memory node is missing"))?
                .set_regs(&[RegInfo::new(region.base, Some(region.size))]);
        }
        Ok(())
    }

    pub(crate) fn patch_chosen(
        &mut self,
        initrd_start_size: Option<(u64, u64)>,
        explicit_cmdline: Option<&str>,
    ) -> AxVmResult {
        let chosen_id = self.ensure_path("/chosen")?;
        let chosen = self
            .fdt
            .node_mut(chosen_id)
            .ok_or_else(|| ax_err_type!(InvalidData, "/chosen node is missing"))?;

        let bootargs = explicit_cmdline
            .or_else(|| {
                chosen
                    .get_property("bootargs")
                    .and_then(|prop| prop.as_str())
            })
            .map(sanitize_bootargs);
        if let Some(bootargs) = bootargs {
            chosen.set_property(prop_string("bootargs", &bootargs));
        }

        chosen.remove_property("linux,initrd-start");
        chosen.remove_property("linux,initrd-end");
        if let Some((start, size)) = initrd_start_size {
            chosen.set_property(prop_u64("linux,initrd-start", start));
            chosen.set_property(prop_u64("linux,initrd-end", start.saturating_add(size)));
        }
        Ok(())
    }

    pub(crate) fn copy_subtree_from(
        &mut self,
        source: &Fdt,
        source_id: NodeId,
        dest_parent: NodeId,
        filter_guest_cpu_props: bool,
    ) -> AxVmResult<NodeId> {
        super::import::copy_subtree(self, source, source_id, dest_parent, filter_guest_cpu_props)
    }

    pub(crate) fn clone_filtered(
        source: &Fdt,
        keep: impl Fn(NodeId, &str, &Node) -> bool,
    ) -> AxVmResult<Self> {
        let mut dest = FdtTree::new();
        dest.fdt.boot_cpuid_phys = source.boot_cpuid_phys;
        dest.fdt.memory_reservations = source.memory_reservations.clone();

        let root_id = source.root_id();
        let root = source
            .node(root_id)
            .ok_or_else(|| ax_err_type!(InvalidData, "source FDT root is missing"))?;
        copy_properties(
            source,
            root,
            dest.fdt.node_mut(dest.fdt.root_id()).unwrap(),
            false,
        );

        let mut stack = Vec::new();
        for child in root.children().iter().rev() {
            stack.push((*child, dest.fdt.root_id()));
        }

        while let Some((source_id, dest_parent)) = stack.pop() {
            let Some(source_node) = source.node(source_id) else {
                continue;
            };
            let path = source.path_of(source_id);
            let node_kept = keep(source_id, &path, source_node);
            let next_parent = if node_kept {
                let new_id = dest.add_node(dest_parent, Node::new(source_node.name()));
                copy_properties(
                    source,
                    source_node,
                    dest.fdt.node_mut(new_id).unwrap(),
                    path.starts_with("/cpus/"),
                );
                new_id
            } else {
                dest_parent
            };

            for child in source_node.children().iter().rev() {
                stack.push((*child, next_parent));
            }
        }

        Ok(dest)
    }

    /// Ensures the guest FDT exposes one `/cpus/cpu@<id>` node per guest
    /// virtual CPU id in `phys_cpu_ids`.
    ///
    /// The CPU nodes are copied from the host FDT, so when the guest has more
    /// vCPUs than host physical CPUs (vCPU over-subscription) some ids have no
    /// host counterpart. For every missing id a fresh CPU node is cloned from
    /// the first host CPU node, named `cpu@<id>` and re-registered with
    /// `reg = <id>`, so that the guest SMP bootstrap sees all CPUs and powers
    /// the secondaries up via PSCI.
    ///
    /// A clone never inherits the template's `phandle`/`linux,phandle`: a
    /// phandle identifies exactly one node, and a cloned CPU is a distinct
    /// guest identity. The replacement value is preferably above every
    /// phandle of the host and guest trees; at the upper bound, the allocator
    /// reuses the smallest available gap without taking over an identity.
    pub(crate) fn ensure_guest_cpu_nodes(
        &mut self,
        host: &Fdt,
        phys_cpu_ids: &[usize],
    ) -> AxVmResult {
        let existing = self
            .node_paths()
            .into_iter()
            .filter_map(|(_, path)| {
                path.strip_prefix("/cpus/cpu@")
                    .and_then(|id| id.split('/').next())
                    .and_then(|id| usize::from_str_radix(id, 16).ok())
            })
            .collect::<BTreeSet<_>>();

        let missing = phys_cpu_ids
            .iter()
            .copied()
            .filter(|id| !existing.contains(id))
            .collect::<Vec<_>>();
        if missing.is_empty() {
            return Ok(());
        }

        let template_id = host
            .iter_node_ids()
            .find(|node_id| host.path_of(*node_id).starts_with("/cpus/cpu@"))
            .ok_or_else(|| ax_err_type!(InvalidInput, "host FDT has no CPU node template"))?;
        let template = host
            .node(template_id)
            .ok_or_else(|| ax_err_type!(InvalidData, "host FDT CPU node template is missing"))?;

        let cpus_id = self.ensure_path("/cpus")?;
        let template_has_affinity = template
            .properties()
            .iter()
            .any(|prop| prop.name() == "mpidr-affinity");
        let guest_cpu_execution_property =
            super::selected_guest_fdt_policy().guest_cpu_execution_property;
        let template_has_phandle = template.get_property("phandle").is_some();
        let template_has_legacy_phandle = template.get_property("linux,phandle").is_some();
        for id in missing {
            let node_id = self.add_node(cpus_id, Node::new(&format!("cpu@{id:x}")));
            for prop in template.properties() {
                if is_phandle_prop(prop.name())
                    || !guest_cpu_execution_property(prop.name())
                    || should_skip_guest_cpu_prop(host, prop.name())
                {
                    continue;
                }
                self.set_property(node_id, prop.clone())?;
            }
            self.fdt
                .view_typed_mut(node_id)
                .ok_or_else(|| ax_err_type!(InvalidData, "new guest CPU node is missing"))?
                .set_regs(&[RegInfo::new(id as u64, None)]);
            // Keep `mpidr-affinity` consistent with the virtualized MPIDR
            // (`VMPIDR_EL2 = 1<<31 | id`) when the host template carries it.
            if template_has_affinity {
                self.set_property(node_id, prop_u32_list("mpidr-affinity", &[id as u32]))?;
            }
            if template_has_phandle {
                let phandle = next_free_phandle(&[host, self.inner()])?;
                self.set_property(node_id, prop_u32_list("phandle", &[phandle]))?;
                if template_has_legacy_phandle {
                    self.set_property(node_id, prop_u32_list("linux,phandle", &[phandle]))?;
                }
            }
        }
        Ok(())
    }

    fn remove_paths_deepest_first(&mut self, mut paths: Vec<String>) {
        paths.sort_by_key(|path| std::cmp::Reverse(path.matches('/').count()));
        for path in paths {
            self.fdt.remove_by_path(&path);
        }
    }
}

pub(super) fn checked_node_phandle(node: &Node) -> AxVmResult<Option<u32>> {
    for name in ["phandle", "linux,phandle"] {
        if let Some(property) = node.get_property(name)
            && property.data.len() != 4
        {
            return Err(ax_err_type!(
                InvalidData,
                format!("FDT node {} has malformed {name}", node.name())
            ));
        }
    }
    let phandle = node.get_property("phandle").and_then(Property::get_u32);
    let linux_phandle = node
        .get_property("linux,phandle")
        .and_then(Property::get_u32);
    if let (Some(phandle), Some(linux_phandle)) = (phandle, linux_phandle)
        && phandle != linux_phandle
    {
        return Err(ax_err_type!(
            InvalidData,
            std::format!(
                "FDT node {} has conflicting phandle values {phandle:#x} and {linux_phandle:#x}",
                node.name()
            )
        ));
    }
    let value = phandle.or(linux_phandle);
    if let Some(value) = value
        && (value == 0 || value == u32::MAX)
    {
        return Err(ax_err_type!(
            InvalidData,
            std::format!("FDT node {} uses reserved phandle {value:#x}", node.name())
        ));
    }
    Ok(value)
}

fn no_free_phandle() -> crate::AxVmError {
    ax_err_type!(InvalidData, "No free firmware phandle")
}

fn first_available_phandle(used: BTreeSet<u32>) -> AxVmResult<u32> {
    let mut candidate = 1_u32;
    for value in used {
        if value < candidate {
            continue;
        }
        if value > candidate {
            break;
        }
        candidate = candidate.checked_add(1).ok_or_else(no_free_phandle)?;
    }
    if candidate == u32::MAX {
        Err(no_free_phandle())
    } else {
        Ok(candidate)
    }
}

pub(crate) fn prop_u64(name: &str, value: u64) -> Property {
    let mut prop = Property::new(name, Vec::new());
    prop.set_u64(value);
    prop
}

pub(crate) fn prop_string(name: &str, value: &str) -> Property {
    let mut prop = Property::new(name, Vec::new());
    prop.set_string(value);
    prop
}

pub(crate) fn host_fdt_bytes_from_ptr(ptr: *const u8) -> Option<&'static [u8]> {
    if ptr.is_null() {
        return None;
    }

    let header = unsafe {
        let bytes = std::slice::from_raw_parts(ptr, std::mem::size_of::<Header>());
        Header::from_bytes(bytes).ok()?
    };

    Some(unsafe { std::slice::from_raw_parts(ptr, header.totalsize as usize) })
}

pub(crate) fn sanitize_bootargs(bootargs: &str) -> String {
    const FSCK_REPAIR_BOOTARG: &str = "fsck.repair=yes";

    let rewritten = bootargs.replace(" ro ", " rw ");
    let tokens = rewritten.split_whitespace().collect::<Vec<_>>();
    let has_fsck_policy = tokens.iter().any(|token| {
        matches!(
            *token,
            "fastboot"
                | "fsck.mode=skip"
                | "forcefsck"
                | "fsck.mode=force"
                | "fsckfix"
                | "fsck.repair=yes"
                | "fsck.repair=no"
        )
    });
    let has_block_root = tokens.iter().any(|token| {
        token.starts_with("root=/dev/")
            || token.starts_with("root=PARTLABEL=")
            || token.starts_with("root=LABEL=")
            || token.starts_with("root=UUID=")
            || token.starts_with("root=PARTUUID=")
    });
    let mut sanitized = Vec::with_capacity(tokens.len());
    let mut index = 0;

    while index < tokens.len() {
        if matches!(tokens[index], "root=/dev/ram0" | "rdinit=/init") {
            index += 1;
            continue;
        }

        sanitized.push(tokens[index]);
        index += 1;
    }

    if has_block_root && !has_fsck_policy {
        sanitized.push(FSCK_REPAIR_BOOTARG);
    }

    sanitized.join(" ")
}

/// Returns whether `prop_name` names the phandle of the node itself.
///
/// Both spellings identify one node, so a node cloned from a template must not
/// inherit either of them.
fn is_phandle_prop(prop_name: &str) -> bool {
    matches!(prop_name, "phandle" | "linux,phandle")
}

/// Returns the phandle a node advertises, preferring `phandle` over the legacy
/// `linux,phandle`.
fn node_phandle(node: &Node) -> Option<u32> {
    node.get_property("phandle")
        .or_else(|| node.get_property("linux,phandle"))
        .and_then(Property::get_u32)
}

/// Allocates a valid phandle unused by any tree in `trees`.
///
/// Every tree counts, so a caller that clones a node of one tree into another
/// cannot pick a value that a remaining reference of either tree still means.
/// Prefer a value above the highest handle; at the upper bound, reuse a gap.
pub(crate) fn next_free_phandle(trees: &[&Fdt]) -> AxVmResult<u32> {
    let used = trees
        .iter()
        .flat_map(|fdt| {
            fdt.iter_node_ids()
                .filter_map(|node_id| fdt.node(node_id).and_then(node_phandle))
        })
        .collect::<BTreeSet<_>>();
    let highest = used.last().copied().unwrap_or(0);
    if let Some(next) = highest.checked_add(1).filter(|next| *next < u32::MAX) {
        return Ok(next);
    }

    let mut next = 1_u32;
    for phandle in used {
        if phandle > next {
            break;
        }
        if phandle == next {
            next = next
                .checked_add(1)
                .ok_or_else(|| ax_err_type!(InvalidData, "no valid FDT phandle available"))?;
        }
    }
    (next < u32::MAX)
        .then_some(next)
        .ok_or_else(|| ax_err_type!(InvalidData, "no valid FDT phandle available"))
}

fn should_skip_guest_cpu_prop(source: &Fdt, prop_name: &str) -> bool {
    matches!(
        prop_name,
        "riscv,cbop-block-size" | "riscv,cboz-block-size" | "riscv,cbom-block-size"
    ) || (is_roc_rk3568(source)
        && matches!(
            prop_name,
            "operating-points-v2" | "#cooling-cells" | "dynamic-power-coefficient" | "cpu-supply"
        ))
}

pub(crate) fn is_guest_cpu_execution_property(prop_name: &str) -> bool {
    matches!(
        prop_name,
        "#address-cells"
            | "#size-cells"
            | "device_type"
            | "compatible"
            | "reg"
            | "enable-method"
            | "phandle"
            | "linux,phandle"
            | "capacity-dmips-mhz"
            | "clock-frequency"
            | "status"
    )
}

fn is_roc_rk3568(source: &Fdt) -> bool {
    source
        .node(source.root_id())
        .and_then(|root| root.get_property("compatible"))
        .is_some_and(|compatible| {
            compatible.as_str_iter().any(|value| {
                matches!(
                    value,
                    "rockchip,rk3568-firefly-roc-pc" | "rockchip,rk3568-firefly-roc-pc-se"
                )
            })
        })
}

pub(super) fn copy_properties(
    source_fdt: &Fdt,
    source: &Node,
    dest: &mut Node,
    filter_cpu_props: bool,
) {
    for prop in source.properties() {
        if filter_cpu_props && should_skip_guest_cpu_prop(source_fdt, prop.name()) {
            continue;
        }
        dest.set_property(prop.clone());
    }
}

fn prop_u32_list(name: &str, values: &[u32]) -> Property {
    let mut property = Property::new(name, Vec::new());
    property.set_u32_ls(values);
    property
}
