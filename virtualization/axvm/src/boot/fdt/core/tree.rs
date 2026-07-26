use alloc::{format, string::String, vec::Vec};

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

/// GIC SPI ids begin at 32; the DTB `interrupts` cell stores the SPI number
/// (`irq_id - 32`).
const SPI_BASE: usize = 32;
/// DT IRQ trigger type for "edge rising" (matches `InterruptTriggerMode::Edge`
/// used by the virtio-mmio device).
const IRQ_TYPE_EDGE_RISING: u32 = 1;
/// GIC `#interrupt-cells` expected for the 3-cell SPI specifier `<0 spi trigger>`.
const GIC_INTERRUPT_CELLS: u32 = 3;

/// A checked guest-physical MMIO range used for virtio_mmio reg-overlap checks.
#[derive(Debug, Clone, Copy)]
struct CoreRange {
    base: u64,
    end: u64,
}

impl CoreRange {
    fn new(base: u64, size: u64) -> AxVmResult<Self> {
        let end = base
            .checked_add(size)
            .ok_or_else(|| ax_err_type!(InvalidData, "virtio_mmio reg overflows u64"))?;
        Ok(Self { base, end })
    }

    fn overlaps(&self, other: &CoreRange) -> bool {
        self.base < other.end && other.base < self.end
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

    /// Appends one `virtio_mmio@<base>` node per supplied device so the guest
    /// can enumerate virtio-mmio net devices from its device tree.
    ///
    /// Each `(base, size, irq)` tuple describes one device. `irq` is the GIC SPI
    /// id (`>= 32`); the node's `interrupts` property encodes the 3-cell GIC
    /// specifier `<0 (irq - 32) 1>` (SPI, edge-rising). `reg` is encoded with the
    /// root `#address-cells`/`#size-cells`. `interrupt-parent` points at the
    /// guest GIC interrupt-controller (its phandle is allocated if absent).
    ///
    /// Only virtio-net devices reach this path; other emulated device kinds are
    /// not turned into virtio-mmio nodes (plan section 4).
    pub(crate) fn add_virtio_mmio_nodes(&mut self, devices: &[(u64, u64, usize)]) -> AxVmResult {
        if devices.is_empty() {
            return Ok(());
        }
        let gic_phandle = self.resolve_gic_interrupt_parent()?;

        let root = self.fdt.root_id();
        let mut used = self.existing_reg_ranges()?;
        let mut used_irqs = self.existing_gic_spis(gic_phandle);
        for &(base, size, irq) in devices {
            let name = format!("virtio_mmio@{base:x}");
            let range = CoreRange::new(base, size)?;

            if self.fdt.get_by_path_id(&format!("/{name}")).is_some() {
                return Err(ax_err_type!(
                    InvalidData,
                    format!("duplicate virtio_mmio node name {name}")
                ));
            }

            // Reject same-name and overlapping-reg conflicts instead of silently
            // shadowing an existing node (plan section 4).
            for (existing_name, existing_range) in &used {
                if *existing_name == name {
                    return Err(ax_err_type!(
                        InvalidData,
                        format!("duplicate virtio_mmio node name {name}")
                    ));
                }
                if existing_range.overlaps(&range) {
                    return Err(ax_err_type!(
                        InvalidData,
                        format!("virtio_mmio@{base:x} reg {range:?} overlaps {existing_name}")
                    ));
                }
            }

            let spi = irq
                .checked_sub(SPI_BASE)
                .ok_or_else(|| ax_err_type!(InvalidData, format!("irq {irq} is not an SPI")))?;
            if !used_irqs.insert(irq) {
                return Err(ax_err_type!(
                    InvalidData,
                    format!("virtio_mmio IRQ {irq} conflicts with an existing DTB device")
                ));
            }
            let node_id = self.fdt.add_node(root, Node::new(&name));

            let mut compatible = Property::new("compatible", Vec::new());
            compatible.set_string("virtio,mmio");
            self.set_property(node_id, compatible)?;

            self.fdt
                .view_typed_mut(node_id)
                .ok_or_else(|| ax_err_type!(InvalidData, "new virtio_mmio node is missing"))?
                .set_regs(&[RegInfo::new(base, Some(size))]);

            let mut interrupts = Property::new("interrupts", Vec::new());
            interrupts.set_u32_ls(&[0, spi as u32, IRQ_TYPE_EDGE_RISING]);
            self.set_property(node_id, interrupts)?;

            let mut interrupt_parent = Property::new("interrupt-parent", Vec::new());
            interrupt_parent.set_u32_ls(&[gic_phandle]);
            self.set_property(node_id, interrupt_parent)?;

            used.push((name, range));
        }
        Ok(())
    }

    fn existing_reg_ranges(&self) -> AxVmResult<Vec<(String, CoreRange)>> {
        let mut ranges = Vec::new();
        for (node_id, path) in self.node_paths() {
            let Some(node) = self.fdt.view_typed(node_id) else {
                continue;
            };
            for reg in node.regs() {
                let Some(size) = reg.size else {
                    continue;
                };
                if size == 0 {
                    continue;
                }
                ranges.push((path.clone(), CoreRange::new(reg.address, size)?));
            }
        }
        Ok(ranges)
    }

    fn existing_gic_spis(&self, gic_phandle: u32) -> alloc::collections::BTreeSet<usize> {
        let mut irqs = alloc::collections::BTreeSet::new();
        for node_id in self.fdt.iter_node_ids() {
            let Some(node) = self.fdt.node(node_id) else {
                continue;
            };
            if node.interrupt_parent().map(|value| value.raw()) != Some(gic_phandle) {
                continue;
            }
            let Some(interrupts) = node.get_property("interrupts") else {
                continue;
            };
            let cells: Vec<u32> = interrupts.get_u32_iter().collect();
            for spec in cells.chunks_exact(GIC_INTERRUPT_CELLS as usize) {
                if spec[0] == 0 {
                    irqs.insert(SPI_BASE + spec[1] as usize);
                }
            }
        }
        irqs
    }

    /// Resolves the phandle of the guest GIC interrupt-controller.
    ///
    /// Returns the controller's existing phandle, or allocates a fresh
    /// non-conflicting one and writes it back. Verifies `#interrupt-cells == 3`
    /// so the 3-cell `interrupts` specifier encoded above is decodable.
    fn resolve_gic_interrupt_parent(&mut self) -> AxVmResult<u32> {
        // Collect node ids first to avoid borrowing self.fdt while iterating it.
        let ids: Vec<NodeId> = self.fdt.iter_node_ids().collect();
        let mut gic_ids = ids.iter().copied().filter(|id| {
            self.fdt.node(*id).is_some_and(|node| {
                node.is_interrupt_controller()
                    && node
                        .compatibles()
                        .any(|value| value.starts_with("arm,gic-"))
            })
        });
        let gic_id = gic_ids.next().ok_or_else(|| {
            ax_err_type!(
                InvalidData,
                "guest DTB has no ARM GIC interrupt-controller for virtio-mmio interrupts"
            )
        })?;
        if gic_ids.next().is_some() {
            return Err(ax_err_type!(
                InvalidData,
                "guest DTB has multiple ARM GIC interrupt-controllers"
            ));
        }

        let cells = self.fdt.node(gic_id).and_then(Node::interrupt_cells);
        if cells != Some(GIC_INTERRUPT_CELLS) {
            return Err(ax_err_type!(
                InvalidData,
                format!("guest GIC #interrupt-cells = {cells:?}, expected {GIC_INTERRUPT_CELLS}")
            ));
        }

        if let Some(phandle) = self.fdt.node(gic_id).and_then(Node::phandle) {
            return Ok(phandle.raw());
        }

        // Allocate the next phandle after the largest one currently in use.
        let next = ids
            .iter()
            .copied()
            .filter_map(|id| self.fdt.node(id).and_then(Node::phandle).map(|p| p.raw()))
            .max()
            .unwrap_or(0)
            + 1;
        let mut phandle_prop = Property::new("phandle", Vec::new());
        phandle_prop.set_u32_ls(&[next]);
        self.set_property(gic_id, phandle_prop)?;
        Ok(next)
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

#[cfg(test)]
mod virtio_mmio_fdt_tests {
    use super::*;

    fn prop_u32(name: &str, value: u32) -> Property {
        let mut prop = Property::new(name, Vec::new());
        prop.set_u32_ls(&[value]);
        prop
    }

    fn fdt_with_gic_phandle(phandle: u32) -> Fdt {
        let mut fdt = Fdt::new();
        let root = fdt.root_id();
        fdt.node_mut(root)
            .unwrap()
            .set_property(prop_u32("#address-cells", 2));
        fdt.node_mut(root)
            .unwrap()
            .set_property(prop_u32("#size-cells", 2));
        let gic = fdt.add_node(root, Node::new("intc@8000000"));
        let mut compatible = Property::new("compatible", Vec::new());
        compatible.set_string("arm,gic-v3");
        fdt.node_mut(gic).unwrap().set_property(compatible);
        fdt.node_mut(gic)
            .unwrap()
            .set_property(prop_u32("#interrupt-cells", 3));
        fdt.node_mut(gic)
            .unwrap()
            .set_property(Property::new("interrupt-controller", Vec::new()));
        fdt.node_mut(gic)
            .unwrap()
            .set_property(prop_u32("phandle", phandle));
        fdt
    }

    #[test]
    fn virtio_mmio_node_encodes_interrupts_and_parent() {
        let fdt = fdt_with_gic_phandle(0x10);
        let mut tree = FdtTree::from_fdt(fdt);
        tree.add_virtio_mmio_nodes(&[(0x0a00_0000, 0x200, 65)])
            .unwrap();
        let bytes = tree.finish();

        let round = Fdt::from_bytes(&bytes).unwrap();
        let node_id = round
            .get_by_path_id("/virtio_mmio@a000000")
            .expect("virtio_mmio node generated");
        let node = round.node(node_id).unwrap();

        assert_eq!(
            node.interrupt_parent().map(|phandle| phandle.raw()),
            Some(0x10)
        );
        let interrupts = node.get_property("interrupts").expect("interrupts present");
        let values: alloc::vec::Vec<u32> = interrupts.get_u32_iter().collect();
        assert_eq!(values, [0u32, 33, IRQ_TYPE_EDGE_RISING]);
    }

    #[test]
    fn virtio_mmio_rejects_non_spi_irq() {
        let fdt = fdt_with_gic_phandle(0x10);
        let mut tree = FdtTree::from_fdt(fdt);
        assert!(
            tree.add_virtio_mmio_nodes(&[(0x0a00_0000, 0x200, 16)])
                .is_err()
        );
    }
}
