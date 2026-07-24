use alloc::{collections::BTreeSet, format, string::String, vec::Vec};

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

    pub(crate) fn prune_cpu_topology(&mut self) {
        const CPU_MAP_PATH: &str = "/cpus/cpu-map";

        let retained_cpu_phandles = self
            .node_paths()
            .into_iter()
            .filter(|(_, path)| is_direct_cpu_node(path))
            .filter_map(|(id, _)| self.fdt.node(id)?.phandle().map(|phandle| phandle.raw()))
            .collect::<BTreeSet<_>>();
        let mut topology_paths = self
            .node_paths()
            .into_iter()
            .filter_map(|(_, path)| {
                (path == CPU_MAP_PATH || path.starts_with("/cpus/cpu-map/")).then_some(path)
            })
            .collect::<Vec<_>>();
        topology_paths.sort_by_key(|path| core::cmp::Reverse(path.matches('/').count()));

        for path in topology_paths {
            let Some(node_id) = self.fdt.get_by_path_id(&path) else {
                continue;
            };
            let Some(node) = self.fdt.node(node_id) else {
                continue;
            };
            if !node.children().is_empty() {
                continue;
            }
            let references_retained_cpu = node
                .get_property("cpu")
                .and_then(Property::get_u32)
                .is_some_and(|phandle| retained_cpu_phandles.contains(&phandle));
            if !references_retained_cpu {
                self.fdt.remove_by_path(&path);
            }
        }
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

    pub(crate) fn patch_chosen(&mut self, initrd_start_size: Option<(u64, u64)>) -> AxVmResult {
        let chosen_id = self.ensure_path("/chosen")?;
        let chosen = self
            .fdt
            .node_mut(chosen_id)
            .ok_or_else(|| ax_err_type!(InvalidData, "/chosen node is missing"))?;

        if let Some(bootargs) = chosen
            .get_property("bootargs")
            .and_then(|prop| prop.as_str())
            .map(sanitize_bootargs)
        {
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
        skip_cpu_cache_props: bool,
    ) -> AxVmResult<NodeId> {
        let source_node = source
            .node(source_id)
            .ok_or_else(|| ax_err_type!(InvalidData, "source FDT node id is invalid"))?;
        let dest_id = self.add_node(dest_parent, Node::new(source_node.name()));
        copy_properties(
            source_node,
            self.fdt.node_mut(dest_id).unwrap(),
            skip_cpu_cache_props,
        );

        for child_id in source_node.children() {
            self.copy_subtree_from(source, *child_id, dest_id, skip_cpu_cache_props)?;
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
        copy_properties(root, dest.fdt.node_mut(dest.fdt.root_id()).unwrap(), false);

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

    fn remove_paths_deepest_first(&mut self, mut paths: Vec<String>) {
        paths.sort_by_key(|path| core::cmp::Reverse(path.matches('/').count()));
        for path in paths {
            self.fdt.remove_by_path(&path);
        }
    }
}

fn is_direct_cpu_node(path: &str) -> bool {
    path.strip_prefix("/cpus/")
        .is_some_and(|name| name.starts_with("cpu@") && !name.contains('/'))
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
        let bytes = core::slice::from_raw_parts(ptr, core::mem::size_of::<Header>());
        Header::from_bytes(bytes).ok()?
    };

    Some(unsafe { core::slice::from_raw_parts(ptr, header.totalsize as usize) })
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

pub(crate) fn should_skip_guest_cpu_prop(prop_name: &str) -> bool {
    matches!(
        prop_name,
        "riscv,cbop-block-size" | "riscv,cboz-block-size" | "riscv,cbom-block-size"
    )
}

fn copy_properties(source: &Node, dest: &mut Node, skip_cpu_cache_props: bool) {
    for prop in source.properties() {
        if skip_cpu_cache_props && should_skip_guest_cpu_prop(prop.name()) {
            continue;
        }
        dest.set_property(prop.clone());
    }
}
