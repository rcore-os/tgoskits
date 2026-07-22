use alloc::{format, string::String, vec::Vec};

use fdt_edit::{Fdt, Node, NodeId, Property};
use fdt_raw::RegInfo;

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

    pub(crate) fn remove_path(&mut self, path: &str) {
        self.fdt.remove_by_path(path);
    }

    pub(crate) fn remove_properties(
        &mut self,
        node_id: NodeId,
        property_names: &[&str],
    ) -> AxVmResult {
        let node = self
            .fdt
            .node_mut(node_id)
            .ok_or_else(|| ax_err_type!(InvalidData, "FDT node id is invalid"))?;
        for property_name in property_names {
            node.remove_property(property_name);
        }
        Ok(())
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
        {
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
        }

        self.remove_properties(chosen_id, &["linux,initrd-start", "linux,initrd-end"])?;
        if let Some((start, size)) = initrd_start_size {
            let chosen = self
                .fdt
                .node_mut(chosen_id)
                .ok_or_else(|| ax_err_type!(InvalidData, "/chosen node is missing"))?;
            chosen.set_property(prop_u64("linux,initrd-start", start));
            chosen.set_property(prop_u64("linux,initrd-end", start.saturating_add(size)));
        }
        Ok(())
    }

    fn remove_paths_deepest_first(&mut self, mut paths: Vec<String>) {
        paths.sort_by_key(|path| core::cmp::Reverse(path.matches('/').count()));
        for path in paths {
            self.remove_path(&path);
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
