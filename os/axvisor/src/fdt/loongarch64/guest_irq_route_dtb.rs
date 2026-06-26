use alloc::{
    collections::{BTreeMap, BTreeSet},
    format,
    vec::Vec,
};

use ax_errno::{AxResult, ax_err_type};
use axvm::config::AxVMConfig;
use fdt_parser::{Fdt, Node, Status};

use crate::guest_platform::loongarch64::{
    LoongArchGuestIrqRoute, store_guest_irq_routes as store_loongarch_guest_irq_routes,
};

fn property_u32_list(node: &Node<'_>, name: &str) -> Vec<u32> {
    node.find_property(name)
        .map(|prop| prop.u32_list().collect())
        .unwrap_or_default()
}

fn property_u32(node: &Node<'_>, name: &str) -> Option<u32> {
    node.find_property(name)
        .and_then(|prop| prop.u32_list().next())
}

fn node_phandle(node: &Node<'_>) -> Option<usize> {
    node.phandle().map(|phandle| phandle.as_usize())
}

fn interrupt_parent_phandle(node: &Node<'_>) -> Option<usize> {
    node.interrupt_parent()
        .and_then(|parent| node_phandle(&parent.node))
}

fn node_interrupt_cells(node: &Node<'_>) -> usize {
    property_u32(node, "#interrupt-cells").unwrap_or(1) as usize
}

fn node_address_cells(node: &Node<'_>) -> usize {
    property_u32(node, "#address-cells").unwrap_or(2) as usize
}

fn has_compatible(node: &Node<'_>, needle: &str) -> bool {
    node.compatibles().any(|compatible| compatible == needle)
}

fn has_compatible_part(node: &Node<'_>, needle: &str) -> bool {
    node.compatibles()
        .any(|compatible| compatible.contains(needle))
}

fn is_interrupt_controller(node: &Node<'_>) -> bool {
    node.find_property("interrupt-controller").is_some()
}

fn is_enabled_node(node: &Node<'_>) -> bool {
    !matches!(node.status(), Some(Status::Disabled))
}

fn interrupt_ids_for_parent(node: &Node<'_>, parent_phandle: usize) -> Vec<usize> {
    if interrupt_parent_phandle(node) != Some(parent_phandle) {
        return Vec::new();
    }

    node.interrupts()
        .map(|interrupts| {
            interrupts
                .filter_map(|mut interrupt| interrupt.next().map(|id| id as usize))
                .collect()
        })
        .unwrap_or_default()
}

fn interrupts_extended_for_parent(
    node: &Node<'_>,
    phandle_nodes: &BTreeMap<usize, Node<'_>>,
    parent_phandle: usize,
) -> Vec<usize> {
    let cells = property_u32_list(node, "interrupts-extended");
    if cells.is_empty() {
        return Vec::new();
    }

    let mut result = Vec::new();
    let mut index = 0usize;
    while index < cells.len() {
        let phandle = cells[index] as usize;
        index += 1;

        let interrupt_cells = phandle_nodes
            .get(&phandle)
            .map(node_interrupt_cells)
            .unwrap_or(1);

        if index + interrupt_cells > cells.len() {
            warn!(
                "LoongArch interrupts-extended in node {} has truncated tuple at cell {}",
                node.name(),
                index - 1
            );
            break;
        }

        if phandle == parent_phandle && interrupt_cells > 0 {
            result.push(cells[index] as usize);
        }
        index += interrupt_cells;
    }

    result
}

fn direct_interrupt_ids_for_parent(
    node: &Node<'_>,
    phandle_nodes: &BTreeMap<usize, Node<'_>>,
    parent_phandle: usize,
) -> Vec<usize> {
    let mut ids = interrupt_ids_for_parent(node, parent_phandle);
    ids.extend(interrupts_extended_for_parent(
        node,
        phandle_nodes,
        parent_phandle,
    ));
    ids
}

fn first_direct_interrupt_id_for_parent(
    node: &Node<'_>,
    phandle_nodes: &BTreeMap<usize, Node<'_>>,
    parent_phandle: usize,
) -> Option<usize> {
    direct_interrupt_ids_for_parent(node, phandle_nodes, parent_phandle)
        .into_iter()
        .next()
}

fn add_loongarch_irq_route(
    routes: &mut BTreeSet<LoongArchGuestIrqRoute>,
    physical_irq: usize,
    guest_vector: usize,
    source: &str,
) {
    let route = LoongArchGuestIrqRoute {
        physical_irq,
        guest_vector,
    };
    if routes.insert(route) {
        debug!(
            "LoongArch guest IRQ route from DTB: physical_irq={}, guest_vector={}, source={}",
            physical_irq, guest_vector, source
        );
    }
}

fn parse_loongarch_pci_interrupt_map(
    node: &Node<'_>,
    phandle_nodes: &BTreeMap<usize, Node<'_>>,
    pch_pic_phandle: usize,
    pch_pic_base_vec: usize,
    eiointc_guest_vector: usize,
    routes: &mut BTreeSet<LoongArchGuestIrqRoute>,
) {
    let interrupt_map = property_u32_list(node, "interrupt-map");
    if interrupt_map.is_empty() {
        return;
    }

    let child_address_cells = node_address_cells(node);
    let child_interrupt_cells = node_interrupt_cells(node);
    let child_cells = child_address_cells + child_interrupt_cells;
    if child_cells == 0 {
        warn!(
            "LoongArch PCI interrupt-map in node {} has invalid child cell count",
            node.name()
        );
        return;
    }
    let mut index = 0usize;

    while index + child_cells < interrupt_map.len() {
        let parent_phandle = interrupt_map[index + child_cells] as usize;
        let Some(parent_node) = phandle_nodes.get(&parent_phandle) else {
            warn!(
                "LoongArch PCI interrupt-map parent phandle {:#x} not found in node {}",
                parent_phandle,
                node.name()
            );
            break;
        };
        let parent_interrupt_cells =
            pci_interrupt_map_parent_cells(&interrupt_map, child_cells, parent_node);
        let entry_cells = child_cells + 1 + parent_interrupt_cells;
        if index + entry_cells > interrupt_map.len() {
            warn!(
                "LoongArch PCI interrupt-map in node {} has truncated entry at cell {}",
                node.name(),
                index
            );
            break;
        }

        if parent_phandle == pch_pic_phandle && parent_interrupt_cells > 0 {
            let parent_irq = interrupt_map[index + child_cells + 1] as usize;
            add_loongarch_irq_route(
                routes,
                pch_pic_base_vec + parent_irq,
                eiointc_guest_vector,
                node.name(),
            );
        }
        index += entry_cells;
    }
}

fn pci_interrupt_map_parent_cells(
    interrupt_map: &[u32],
    child_cells: usize,
    parent_node: &Node<'_>,
) -> usize {
    let default_parent_cells = node_interrupt_cells(parent_node);
    let default_entry_cells = child_cells + 1 + default_parent_cells;
    let one_cell_entry_cells = child_cells + 2;

    if default_parent_cells > 1
        && interrupt_map.len() % default_entry_cells != 0
        && interrupt_map.len() % one_cell_entry_cells == 0
    {
        1
    } else {
        default_parent_cells
    }
}

pub fn parse_guest_irq_routes(vm_cfg: &AxVMConfig, dtb: &[u8]) -> AxResult {
    let fdt = Fdt::from_bytes(dtb).map_err(|e| {
        ax_err_type!(
            InvalidData,
            format!("Failed to parse DTB image while reading LoongArch IRQ routes: {e:#?}")
        )
    })?;
    let all_nodes: Vec<_> = fdt.all_nodes().collect();
    let mut phandle_nodes = BTreeMap::new();
    let mut eiointc = None;
    let mut pch_pic = None;
    let mut pch_msi_nodes = Vec::new();

    for node in &all_nodes {
        if let Some(phandle) = node_phandle(node) {
            phandle_nodes.insert(phandle, node.clone());
        }
        if has_compatible(node, "loongson,ls2k2000-eiointc") {
            eiointc = Some(node.clone());
        }
        if has_compatible_part(node, "pch-pic") {
            pch_pic = Some(node.clone());
        }
        if has_compatible(node, "loongson,pch-msi-1.0") {
            pch_msi_nodes.push(node.clone());
        }
    }

    let Some(eiointc) = eiointc else {
        warn!(
            "VM[{}] LoongArch guest IRQ route parsing skipped: EIOINTC node not found",
            vm_cfg.id()
        );
        store_loongarch_guest_irq_routes(vm_cfg.id(), Vec::new());
        return Ok(());
    };
    let Some(pch_pic) = pch_pic else {
        warn!(
            "VM[{}] LoongArch guest IRQ route parsing skipped: PCH-PIC node not found",
            vm_cfg.id()
        );
        store_loongarch_guest_irq_routes(vm_cfg.id(), Vec::new());
        return Ok(());
    };

    let Some(pch_pic_phandle) = node_phandle(&pch_pic) else {
        warn!(
            "VM[{}] LoongArch guest IRQ route parsing skipped: PCH-PIC phandle not found",
            vm_cfg.id()
        );
        store_loongarch_guest_irq_routes(vm_cfg.id(), Vec::new());
        return Ok(());
    };
    let Some(eiointc_parent_phandle) = interrupt_parent_phandle(&eiointc) else {
        warn!(
            "VM[{}] LoongArch guest IRQ route parsing skipped: EIOINTC interrupt parent not found",
            vm_cfg.id()
        );
        store_loongarch_guest_irq_routes(vm_cfg.id(), Vec::new());
        return Ok(());
    };
    let Some(eiointc_guest_vector) =
        first_direct_interrupt_id_for_parent(&eiointc, &phandle_nodes, eiointc_parent_phandle)
    else {
        warn!(
            "VM[{}] LoongArch guest IRQ route parsing skipped: EIOINTC CPU vector not found",
            vm_cfg.id()
        );
        store_loongarch_guest_irq_routes(vm_cfg.id(), Vec::new());
        return Ok(());
    };

    let pch_pic_base_vec = property_u32(&pch_pic, "loongson,pic-base-vec").unwrap_or(0) as usize;
    let mut routes = BTreeSet::new();

    for node in &all_nodes {
        if !is_enabled_node(node) || is_interrupt_controller(node) {
            continue;
        }

        for physical_irq in direct_interrupt_ids_for_parent(node, &phandle_nodes, pch_pic_phandle) {
            add_loongarch_irq_route(
                &mut routes,
                pch_pic_base_vec + physical_irq,
                eiointc_guest_vector,
                node.name(),
            );
        }

        parse_loongarch_pci_interrupt_map(
            node,
            &phandle_nodes,
            pch_pic_phandle,
            pch_pic_base_vec,
            eiointc_guest_vector,
            &mut routes,
        );
    }

    for node in pch_msi_nodes {
        let base_vec = property_u32(&node, "loongson,msi-base-vec").unwrap_or(0) as usize;
        let num_vecs = property_u32(&node, "loongson,msi-num-vecs").unwrap_or(0) as usize;
        for physical_irq in base_vec..base_vec.saturating_add(num_vecs) {
            add_loongarch_irq_route(&mut routes, physical_irq, eiointc_guest_vector, node.name());
        }
    }

    let routes = routes.into_iter().collect::<Vec<_>>();
    info!(
        "VM[{}] parsed {} LoongArch guest IRQ route(s) from DTB",
        vm_cfg.id(),
        routes.len()
    );
    debug!(
        "VM[{}] LoongArch guest IRQ routes: {:?}",
        vm_cfg.id(),
        routes
    );
    store_loongarch_guest_irq_routes(vm_cfg.id(), routes);
    Ok(())
}
