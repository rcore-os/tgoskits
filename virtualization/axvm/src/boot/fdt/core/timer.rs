//! Machine-owned AArch64 architectural timer description.

use std::{string::String, vec::Vec};

use fdt_edit::{Fdt, Node, Property};

use super::{
    serial::interrupt_controller_phandle,
    tree::{FdtTree, prop_string},
};
use crate::{
    AxVmResult, ax_err_type,
    machine::{GuestSerialFdtInterrupt, GuestTimerProfile, decode_timer_ppi},
};

const TIMER_COMPATIBLE: &str = "arm,armv8-timer";

/// Returns whether firmware describes a platform timer replaced by the
/// machine-owned architectural timer.
///
/// Devicetree uses the generic `timer` node name for timer devices. The
/// compatibility check also catches malformed or legacy architectural timer
/// nodes that do not follow that naming convention.
pub(crate) fn is_machine_timer_node(node: &Node) -> bool {
    node.name().split('@').next() == Some("timer")
        || node
            .compatibles()
            .any(|compatible| matches!(compatible, "arm,armv8-timer" | "arm,armv7-timer"))
}

/// Reads the host architectural timer's complete interrupt identity.
pub(crate) fn host_timer_profile(fdt: &Fdt) -> AxVmResult<Option<GuestTimerProfile>> {
    let Some(timer_id) = fdt.iter_node_ids().find(|node_id| {
        fdt.node(*node_id).is_some_and(|node| {
            node.compatibles()
                .any(|compatible| compatible == TIMER_COMPATIBLE)
        })
    }) else {
        return Ok(None);
    };
    let timer = fdt
        .view_typed(timer_id)
        .ok_or_else(|| ax_err_type!(InvalidData, "host architectural timer node is missing"))?;
    let node = timer.as_node();
    let interrupt_parent = timer
        .interrupt_parent()
        .ok_or_else(|| {
            ax_err_type!(
                InvalidData,
                "host architectural timer has no interrupt parent"
            )
        })?
        .raw();
    let raw_interrupts = node.get_property("interrupts").ok_or_else(|| {
        ax_err_type!(
            InvalidData,
            "host architectural timer has no interrupts property"
        )
    })?;
    if raw_interrupts.data.len() % (3 * size_of::<u32>()) != 0 {
        return Err(ax_err_type!(
            InvalidData,
            "host architectural timer interrupts contain a partial three-cell specifier"
        ));
    }
    let interrupt_cells = raw_interrupts.get_u32_iter().collect::<Vec<_>>();
    let interrupt_count = interrupt_cells.len() / 3;
    if !(4..=5).contains(&interrupt_count) {
        return Err(ax_err_type!(
            InvalidData,
            std::format!(
                "host architectural timer must describe four or five interrupts, got \
                 {interrupt_count}"
            )
        ));
    }
    let controller = fdt.get_by_phandle(interrupt_parent.into()).ok_or_else(|| {
        ax_err_type!(
            InvalidData,
            std::format!(
                "host architectural timer references missing interrupt parent \
                 {interrupt_parent:#x}"
            )
        )
    })?;
    if controller
        .as_node()
        .get_property("#interrupt-cells")
        .and_then(Property::get_u32)
        != Some(3)
        || !controller
            .as_node()
            .compatibles()
            .any(|compatible| compatible.contains("gic"))
    {
        return Err(ax_err_type!(
            InvalidData,
            "host architectural timer interrupt parent is not a three-cell GIC"
        ));
    }

    let interrupt_specifiers = interrupt_cells
        .chunks_exact(3)
        .map(<[u32]>::to_vec)
        .collect::<Vec<_>>();
    let intids = interrupt_specifiers
        .iter()
        .enumerate()
        .map(|(index, specifier)| {
            decode_timer_ppi(index, specifier).map_err(|error| ax_err_type!(InvalidData, error))
        })
        .collect::<AxVmResult<Vec<_>>>()?;
    let clock_frequency_hz = match node.get_property("clock-frequency").map(Property::get_u32) {
        Some(Some(0) | None) => {
            return Err(ax_err_type!(
                InvalidData,
                "host architectural timer clock-frequency is invalid"
            ));
        }
        Some(Some(frequency)) => Some(frequency),
        None => None,
    };

    Ok(Some(GuestTimerProfile {
        node_path: fdt.path_of(timer_id),
        node_phandle: node
            .get_property("phandle")
            .or_else(|| node.get_property("linux,phandle"))
            .and_then(Property::get_u32),
        interrupt_parent: Some(interrupt_parent),
        interrupt_specifiers,
        secure_physical_intid: intids[0],
        nonsecure_physical_intid: intids[1],
        virtual_intid: intids[2],
        hypervisor_intid: intids[3],
        clock_frequency_hz,
    }))
}

/// Replaces any existing timer node with the standard virtual timer identity.
pub(crate) fn install_machine_timer(
    tree: &mut FdtTree,
    profile: Option<&GuestTimerProfile>,
) -> AxVmResult {
    let Some(profile) = profile else {
        return Ok(());
    };
    profile
        .validated_intids()
        .map_err(|error| ax_err_type!(InvalidData, error))?;

    let timer_paths = tree
        .inner()
        .iter_node_ids()
        .filter_map(|node_id| {
            let node = tree.inner().node(node_id)?;
            is_machine_timer_node(node).then(|| tree.inner().path_of(node_id))
        })
        .collect::<Vec<_>>();
    for path in timer_paths {
        tree.inner_mut().remove_by_path(&path);
    }

    let path = if profile.node_path.is_empty() {
        String::from("/timer")
    } else {
        profile.node_path.clone()
    };
    let (parent_path, node_name) = path.rsplit_once('/').ok_or_else(|| {
        ax_err_type!(
            InvalidData,
            std::format!("architectural timer node path is not absolute: {path}")
        )
    })?;
    if node_name.is_empty() {
        return Err(ax_err_type!(
            InvalidData,
            std::format!("architectural timer node path has no node name: {path}")
        ));
    }
    let parent = if parent_path.is_empty() {
        tree.inner().root_id()
    } else {
        tree.ensure_path(parent_path)?
    };
    let timer = tree.add_node(parent, Node::new(node_name));
    tree.set_property(timer, prop_string("compatible", TIMER_COMPATIBLE))?;
    let interrupt_parent = match profile.interrupt_parent {
        Some(parent) => parent,
        None => interrupt_controller_phandle(tree, GuestSerialFdtInterrupt::GicSpi)?,
    };
    tree.set_property(timer, prop_u32("interrupt-parent", interrupt_parent))?;
    let flattened = profile
        .interrupt_specifiers
        .iter()
        .flatten()
        .copied()
        .collect::<Vec<_>>();
    tree.set_property(timer, prop_u32_list("interrupts", &flattened))?;
    if let Some(frequency) = profile.clock_frequency_hz {
        tree.set_property(timer, prop_u32("clock-frequency", frequency))?;
    }
    if let Some(phandle) = profile.node_phandle {
        if let Some(existing) = tree.inner().get_by_phandle(phandle.into())
            && existing.id() != timer
        {
            return Err(ax_err_type!(
                InvalidData,
                std::format!("architectural timer phandle {phandle:#x} is already in use")
            ));
        }
        tree.set_property(timer, prop_u32("phandle", phandle))?;
        tree.set_property(timer, prop_u32("linux,phandle", phandle))?;
    }
    Ok(())
}

fn prop_u32(name: &str, value: u32) -> Property {
    prop_u32_list(name, &[value])
}

fn prop_u32_list(name: &str, values: &[u32]) -> Property {
    let mut property = Property::new(name, Vec::new());
    property.set_u32_ls(values);
    property
}

#[cfg(test)]
mod tests {
    use std::vec;

    use fdt_edit::{Node, Property};

    use super::*;

    fn host_timer_fdt(interrupts: &[u32], frequency: Option<u32>) -> Fdt {
        let mut fdt = Fdt::new();
        let root = fdt.root_id();
        let gic = fdt.add_node(root, Node::new("intc"));
        let mut compatible = Property::new("compatible", vec![]);
        compatible.set_string("arm,gic-v3");
        fdt.node_mut(gic).unwrap().set_property(compatible);
        fdt.node_mut(gic)
            .unwrap()
            .set_property(Property::new("interrupt-controller", vec![]));
        fdt.node_mut(gic)
            .unwrap()
            .set_property(prop_u32("#interrupt-cells", 3));
        fdt.node_mut(gic)
            .unwrap()
            .set_property(prop_u32("phandle", 7));
        fdt.node_mut(root)
            .unwrap()
            .set_property(prop_u32("interrupt-parent", 7));
        let timer = fdt.add_node(root, Node::new("timer"));
        fdt.node_mut(timer)
            .unwrap()
            .set_property(prop_string("compatible", TIMER_COMPATIBLE));
        fdt.node_mut(timer)
            .unwrap()
            .set_property(prop_u32_list("interrupts", interrupts));
        if let Some(frequency) = frequency {
            fdt.node_mut(timer)
                .unwrap()
                .set_property(prop_u32("clock-frequency", frequency));
        }
        let encoded = fdt.encode();
        Fdt::from_bytes(encoded.as_ref()).unwrap()
    }

    fn reparse(fdt: Fdt) -> Fdt {
        let encoded = fdt.encode();
        Fdt::from_bytes(encoded.as_ref()).unwrap()
    }

    #[test]
    fn parses_four_timer_interrupts_and_optional_frequency() {
        let fdt = host_timer_fdt(
            &[1, 13, 0xf04, 1, 14, 0xf04, 1, 11, 0xf04, 1, 10, 0xf04],
            Some(50_000_000),
        );

        let profile = host_timer_profile(&fdt).unwrap().unwrap();

        assert_eq!(profile.interrupt_parent, Some(7));
        assert_eq!(profile.nonsecure_physical_intid, 30);
        assert_eq!(profile.virtual_intid, 27);
        assert_eq!(profile.clock_frequency_hz, Some(50_000_000));
    }

    #[test]
    fn rejects_a_truncated_timer_interrupt_list() {
        let fdt = host_timer_fdt(&[1, 13, 4, 1, 14, 4, 1, 11, 4], None);

        assert!(host_timer_profile(&fdt).is_err());
    }

    #[test]
    fn parses_the_optional_fifth_timer_interrupt() {
        let fdt = host_timer_fdt(
            &[
                1, 13, 0xf04, 1, 14, 0xf04, 1, 11, 0xf04, 1, 10, 0xf04, 1, 12, 0xf04,
            ],
            None,
        );

        let profile = host_timer_profile(&fdt).unwrap().unwrap();

        assert_eq!(profile.interrupt_specifiers.len(), 5);
        assert_eq!(profile.interrupt_specifiers[4], vec![1, 12, 0xf04]);
    }

    #[test]
    fn rejects_trailing_interrupt_cells_hidden_by_typed_iteration() {
        let fdt = host_timer_fdt(&[1, 13, 4, 1, 14, 4, 1, 11, 4, 1, 10, 4, 1], None);

        assert!(host_timer_profile(&fdt).is_err());
    }

    #[test]
    fn rejects_an_invalid_optional_hyp_virtual_timer_interrupt() {
        let fdt = host_timer_fdt(&[1, 13, 4, 1, 14, 4, 1, 11, 4, 1, 10, 4, 0, 12, 4], None);

        assert!(host_timer_profile(&fdt).is_err());
    }

    #[test]
    fn rejects_non_level_timer_ppi_flags() {
        let fdt = host_timer_fdt(&[1, 13, 4, 1, 14, 4, 1, 11, 1, 1, 10, 4], None);

        assert!(host_timer_profile(&fdt).is_err());
    }

    #[test]
    fn rejects_zero_or_malformed_firmware_frequency() {
        let interrupts = &[1, 13, 4, 1, 14, 4, 1, 11, 4, 1, 10, 4];
        assert!(host_timer_profile(&host_timer_fdt(interrupts, Some(0))).is_err());

        let mut malformed = host_timer_fdt(interrupts, None);
        let timer = malformed.get_by_path_id("/timer").unwrap();
        malformed
            .node_mut(timer)
            .unwrap()
            .set_property(Property::new(
                "clock-frequency",
                vec![0, 0, 0, 1, 0, 0, 0, 2],
            ));
        assert!(host_timer_profile(&reparse(malformed)).is_err());
    }

    #[test]
    fn rejects_missing_or_non_gic_interrupt_parent() {
        let interrupts = &[1, 13, 4, 1, 14, 4, 1, 11, 4, 1, 10, 4];
        let mut missing = host_timer_fdt(interrupts, None);
        missing
            .node_mut(missing.root_id())
            .unwrap()
            .remove_property("interrupt-parent");
        assert!(host_timer_profile(&reparse(missing)).is_err());

        let mut non_gic = host_timer_fdt(interrupts, None);
        let controller = non_gic.get_by_path_id("/intc").unwrap();
        non_gic
            .node_mut(controller)
            .unwrap()
            .set_property(prop_string("compatible", "vendor,other-intc"));
        assert!(host_timer_profile(&reparse(non_gic)).is_err());
    }

    #[test]
    fn installed_timer_drops_host_errata_and_keeps_interrupt_order() {
        let fdt = host_timer_fdt(
            &[1, 13, 0xf04, 1, 14, 0xf04, 1, 11, 0xf04, 1, 10, 0xf04],
            None,
        );
        let profile = host_timer_profile(&fdt).unwrap().unwrap();
        let mut tree = FdtTree::from_fdt(fdt);
        let timer = tree.inner().get_by_path_id("/timer").unwrap();
        tree.set_property(timer, Property::new("arm,no-tick-in-suspend", vec![]))
            .unwrap();

        install_machine_timer(&mut tree, Some(&profile)).unwrap();

        let timer = tree.inner().get_by_path("/timer").unwrap();
        assert_eq!(
            timer
                .as_node()
                .get_property("interrupts")
                .unwrap()
                .get_u32_iter()
                .collect::<Vec<_>>(),
            profile
                .interrupt_specifiers
                .iter()
                .flatten()
                .copied()
                .collect::<Vec<_>>()
        );
        assert!(
            timer
                .as_node()
                .get_property("arm,no-tick-in-suspend")
                .is_none()
        );
        assert!(timer.as_node().get_property("clock-frequency").is_none());
        assert_eq!(
            timer
                .as_node()
                .get_property("interrupt-parent")
                .and_then(Property::get_u32),
            Some(7)
        );
        assert_eq!(
            timer.as_node().compatibles().collect::<Vec<_>>(),
            vec![TIMER_COMPATIBLE]
        );
    }

    #[test]
    fn installed_timer_removes_firmware_platform_timer_nodes() {
        let mut fdt = host_timer_fdt(
            &[1, 13, 0xf04, 1, 14, 0xf04, 1, 11, 0xf04, 1, 10, 0xf04],
            None,
        );
        let profile = host_timer_profile(&fdt).unwrap().unwrap();
        let platform_timer = fdt.add_node(fdt.root_id(), Node::new("timer@10002000"));
        fdt.node_mut(platform_timer)
            .unwrap()
            .set_property(prop_string("compatible", "vendor,soc-timer"));
        let mut tree = FdtTree::from_fdt(fdt);

        install_machine_timer(&mut tree, Some(&profile)).unwrap();

        assert!(tree.inner().get_by_path_id("/timer@10002000").is_none());
        assert!(tree.inner().get_by_path_id("/timer").is_some());
    }
}
