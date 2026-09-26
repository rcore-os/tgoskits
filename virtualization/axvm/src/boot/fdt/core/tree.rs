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
        let source_node = source
            .node(source_id)
            .ok_or_else(|| ax_err_type!(InvalidData, "source FDT node id is invalid"))?;
        let dest_id = self.add_node(dest_parent, Node::new(source_node.name()));
        copy_properties(
            source,
            source_node,
            self.fdt.node_mut(dest_id).unwrap(),
            filter_guest_cpu_props,
        );

        for child_id in source_node.children() {
            self.copy_subtree_from(source, *child_id, dest_id, filter_guest_cpu_props)?;
        }

        Ok(dest_id)
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
    /// phandle identifies exactly one node, and the guest nodes that already
    /// point at the template (for example `/cpus/cpu-map`) must keep resolving
    /// to it. The replacement value is taken above every phandle of the host
    /// tree as well, so a clone cannot take over the identity of a host CPU that
    /// the guest dropped and still refers to.
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
        let template_has_phandle = template.get_property("phandle").is_some();
        let template_has_legacy_phandle = template.get_property("linux,phandle").is_some();
        for id in missing {
            let node_id = self.add_node(cpus_id, Node::new(&format!("cpu@{id:x}")));
            for prop in template.properties() {
                if is_phandle_prop(prop.name()) || should_skip_guest_cpu_prop(host, prop.name()) {
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
                let phandle = next_free_phandle(&[host, self.inner()]);
                self.set_property(node_id, prop_u32_list("phandle", &[phandle]))?;
                if template_has_legacy_phandle {
                    self.set_property(node_id, prop_u32_list("linux,phandle", &[phandle]))?;
                }
            }
        }
        Ok(())
    }

    /// Removes the `/cpus/cpu-map` entries that reference a CPU the guest does
    /// not have.
    ///
    /// `cpu-map` is copied from the host FDT, where it describes every host CPU,
    /// but the guest keeps only the CPU nodes selected by `phys_cpu_ids` and an
    /// over-subscribed vCPU has no host CPU at all. Each entry that lost its CPU,
    /// together with the `core`/`cluster` nodes that only carried it, is dropped
    /// so the guest tree never references a phandle it does not define. Guest SMP
    /// enumeration reads `/cpus/cpu@<id>` with its `reg` and `enable-method`
    /// properties instead of this map, so the guest boots the same way either.
    pub(crate) fn prune_stale_cpu_map_entries(&mut self) -> AxVmResult {
        const CPU_MAP: &str = "/cpus/cpu-map";
        if self.fdt.get_by_path_id(CPU_MAP).is_none() {
            return Ok(());
        }

        let present = self.phandles_in_use();
        let stale_cores = self
            .node_paths()
            .into_iter()
            .filter(|(_, path)| path.starts_with("/cpus/cpu-map/"))
            .filter_map(|(node_id, path)| {
                let referenced = self.fdt.node(node_id)?.get_property("cpu")?;
                (!referenced.get_u32_iter().all(|cpu| present.contains(&cpu))).then_some(path)
            })
            .collect::<Vec<_>>();
        self.remove_paths_deepest_first(stale_cores);

        // A `core` node only exists to point at a CPU, so containers that lost
        // their last child go away as well. Each pass peels one level and ends
        // once nothing is empty any more.
        loop {
            let empty = self
                .node_paths()
                .into_iter()
                .filter(|(_, path)| path == CPU_MAP || path.starts_with("/cpus/cpu-map/"))
                .filter(|(node_id, _)| {
                    self.fdt.node(*node_id).is_some_and(|node| {
                        node.properties().is_empty() && node.children().is_empty()
                    })
                })
                .map(|(_, path)| path)
                .collect::<Vec<_>>();
            if empty.is_empty() {
                return Ok(());
            }
            self.remove_paths_deepest_first(empty);
        }
    }

    /// Returns every phandle that some node of this tree defines.
    ///
    /// The lookup walks the properties instead of the phandle cache, which only
    /// tracks the values seen when the tree was parsed and, on a duplicate, keeps
    /// a single node.
    fn phandles_in_use(&self) -> BTreeSet<u32> {
        self.fdt
            .iter_node_ids()
            .filter_map(|node_id| self.fdt.node(node_id).and_then(node_phandle))
            .collect()
    }

    fn remove_paths_deepest_first(&mut self, mut paths: Vec<String>) {
        paths.sort_by_key(|path| std::cmp::Reverse(path.matches('/').count()));
        for path in paths {
            self.fdt.remove_by_path(&path);
        }
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

/// Returns the smallest phandle that no node of any tree in `trees` uses.
///
/// Every tree counts, so a caller that clones a node of one tree into another
/// cannot pick a value that a remaining reference of either tree still means.
pub(crate) fn next_free_phandle(trees: &[&Fdt]) -> u32 {
    let highest = trees
        .iter()
        .flat_map(|fdt| {
            fdt.iter_node_ids()
                .filter_map(|node_id| fdt.node(node_id).and_then(node_phandle))
        })
        .max()
        .unwrap_or(0);
    highest.saturating_add(1).max(1)
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

fn copy_properties(source_fdt: &Fdt, source: &Node, dest: &mut Node, filter_cpu_props: bool) {
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
