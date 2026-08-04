//! Machine-owned interrupt-controller description for guest device trees.

use axvm_types::EmulatedDeviceType;
use fdt_edit::{Fdt, NodeId, Property};
use fdt_raw::RegInfo;

use super::tree::FdtTree;
use crate::{
    AxVmResult, ax_err_type,
    machine::{GuestGicCpuRegion, GuestGicProfile, GuestMmioRegion, GuestPlicProfile},
};

/// Rewrites the interrupt-controller resources to match the VM-owned controller.
pub(crate) fn install_machine_interrupt_controller(
    tree: &mut FdtTree,
    cpu_num: usize,
    gic_profile: Option<&GuestGicProfile>,
    plic_profile: Option<&GuestPlicProfile>,
) -> AxVmResult {
    if let Some(profile) = plic_profile {
        return install_plic_registers(tree, profile);
    }

    let fallback;
    let profile = match gic_profile {
        Some(profile) => profile,
        None => {
            let machine = crate::machine::current_machine_profile(cpu_num);
            let distributor = machine
                .emulated_devices
                .iter()
                .find(|device| device.emu_type == EmulatedDeviceType::InterruptController);
            let per_cpu = machine
                .emulated_devices
                .iter()
                .find(|device| device.emu_type == EmulatedDeviceType::GicCpuRegion);
            let (Some(distributor), Some(per_cpu)) = (distributor, per_cpu) else {
                return Ok(());
            };
            fallback = GuestGicProfile {
                compatible: "arm,gic-v3".into(),
                node_path: alloc::string::String::new(),
                node_phandle: None,
                distributor: GuestMmioRegion {
                    base: distributor.base_gpa,
                    length: distributor.length,
                },
                cpu_region: GuestGicCpuRegion::Redistributors(GuestMmioRegion {
                    base: per_cpu.base_gpa,
                    length: per_cpu.length,
                }),
            };
            &fallback
        }
    };
    install_gic_registers(tree, profile)
}

/// Reads the host PLIC register window and firmware identity.
pub(crate) fn host_plic_profile(fdt: &Fdt) -> AxVmResult<Option<GuestPlicProfile>> {
    let Some(controller) = find_plic_in_fdt(fdt) else {
        return Ok(None);
    };
    let view = fdt
        .view_typed(controller)
        .ok_or_else(|| ax_err_type!(InvalidData, "host PLIC node is missing"))?;
    let reg = view
        .regs()
        .into_iter()
        .next()
        .ok_or_else(|| ax_err_type!(InvalidData, "host PLIC node has no register range"))?;
    let (base, length) = checked_plic_reg(&reg)?;
    let node = view.as_node();

    Ok(Some(GuestPlicProfile {
        node_path: fdt.path_of(controller),
        node_phandle: node
            .get_property("phandle")
            .or_else(|| node.get_property("linux,phandle"))
            .and_then(Property::get_u32),
        base,
        length,
    }))
}

/// Reads the host GIC register windows and firmware identity.
pub(crate) fn host_gic_profile(fdt: &Fdt) -> AxVmResult<Option<GuestGicProfile>> {
    let Some((controller, compatible)) = fdt.iter_node_ids().find_map(|node_id| {
        let node = fdt.node(node_id)?;
        if node.get_property("interrupt-controller").is_none() {
            return None;
        }
        node.compatibles()
            .find(|compatible| is_supported_gic(compatible))
            .map(|compatible| (node_id, alloc::string::String::from(compatible)))
    }) else {
        return Ok(None);
    };
    let view = fdt
        .view_typed(controller)
        .ok_or_else(|| ax_err_type!(InvalidData, "host GIC node is missing"))?;
    let regs = view.regs();
    if regs.len() < 2 {
        return Err(ax_err_type!(
            InvalidData,
            "host GIC node must provide distributor and per-CPU ranges"
        ));
    }
    let (distributor_base, distributor_length) = checked_reg(&regs[0], "distributor")?;
    let per_cpu_name = if compatible == "arm,gic-v3" {
        "redistributor"
    } else {
        "CPU interface"
    };
    let (per_cpu_base, per_cpu_length) = checked_reg(&regs[1], per_cpu_name)?;
    let node = view.as_node();
    let per_cpu = GuestMmioRegion {
        base: per_cpu_base,
        length: per_cpu_length,
    };

    Ok(Some(GuestGicProfile {
        compatible,
        node_path: fdt.path_of(controller),
        node_phandle: node
            .get_property("phandle")
            .or_else(|| node.get_property("linux,phandle"))
            .and_then(Property::get_u32),
        distributor: GuestMmioRegion {
            base: distributor_base,
            length: distributor_length,
        },
        cpu_region: if node
            .compatibles()
            .any(|compatible| compatible == "arm,gic-v3")
        {
            GuestGicCpuRegion::Redistributors(per_cpu)
        } else {
            GuestGicCpuRegion::CpuInterface(per_cpu)
        },
    }))
}

/// Reads the host VGIC maintenance PPI INTID.
pub(crate) fn host_gic_maintenance_intid(fdt: &Fdt) -> AxVmResult<Option<u32>> {
    let Some(controller) = find_host_gic(fdt) else {
        return Ok(None);
    };
    let node = fdt
        .node(controller)
        .ok_or_else(|| ax_err_type!(InvalidData, "host GIC node is missing"))?;
    if node
        .get_property("#interrupt-cells")
        .and_then(Property::get_u32)
        != Some(3)
    {
        return Err(ax_err_type!(
            InvalidData,
            "host GIC maintenance interrupt requires three interrupt cells"
        ));
    }
    let Some(interrupts) = node.get_property("interrupts") else {
        return Ok(None);
    };
    let mut cells = interrupts.as_reader();
    let interrupt_type = cells.read_u32().ok_or_else(|| {
        ax_err_type!(
            InvalidData,
            "host GIC maintenance interrupt type is missing"
        )
    })?;
    let source = cells.read_u32().ok_or_else(|| {
        ax_err_type!(
            InvalidData,
            "host GIC maintenance interrupt source is missing"
        )
    })?;
    cells.read_u32().ok_or_else(|| {
        ax_err_type!(
            InvalidData,
            "host GIC maintenance interrupt flags are missing"
        )
    })?;

    if interrupt_type != 1 || source >= 16 {
        return Err(ax_err_type!(
            Unsupported,
            alloc::format!(
                "host GIC maintenance interrupt must be a PPI, got type {interrupt_type} source \
                 {source}"
            )
        ));
    }
    Ok(Some(16 + source))
}

fn checked_reg(reg: &fdt_edit::RegFixed, name: &str) -> AxVmResult<(usize, usize)> {
    let base = usize::try_from(reg.address).map_err(|_| {
        ax_err_type!(
            InvalidData,
            alloc::format!("host GIC {name} address does not fit usize")
        )
    })?;
    let length = reg
        .size
        .ok_or_else(|| {
            ax_err_type!(
                InvalidData,
                alloc::format!("host GIC {name} range has no size")
            )
        })
        .and_then(|length| {
            usize::try_from(length).map_err(|_| {
                ax_err_type!(
                    InvalidData,
                    alloc::format!("host GIC {name} range size does not fit usize")
                )
            })
        })?;
    if length == 0 {
        return Err(ax_err_type!(
            InvalidData,
            alloc::format!("host GIC {name} range is empty")
        ));
    }
    Ok((base, length))
}

fn checked_plic_reg(reg: &fdt_edit::RegFixed) -> AxVmResult<(usize, usize)> {
    let base = usize::try_from(reg.address)
        .map_err(|_| ax_err_type!(InvalidData, "host PLIC address does not fit usize"))?;
    let length = reg
        .size
        .ok_or_else(|| ax_err_type!(InvalidData, "host PLIC range has no size"))
        .and_then(|length| {
            usize::try_from(length)
                .map_err(|_| ax_err_type!(InvalidData, "host PLIC range size does not fit usize"))
        })?;
    if length == 0 {
        return Err(ax_err_type!(InvalidData, "host PLIC range is empty"));
    }
    Ok((base, length))
}

fn install_gic_registers(tree: &mut FdtTree, profile: &GuestGicProfile) -> AxVmResult {
    match (&*profile.compatible, profile.cpu_region) {
        ("arm,gic-v3", GuestGicCpuRegion::Redistributors(_))
        | ("arm,cortex-a15-gic" | "arm,gic-400", GuestGicCpuRegion::CpuInterface(_)) => {}
        _ => {
            return Err(ax_err_type!(
                InvalidData,
                "guest GIC compatible string does not match its per-CPU register model"
            ));
        }
    }
    let controller = (!profile.node_path.is_empty())
        .then(|| tree.inner().get_by_path_id(&profile.node_path))
        .flatten()
        .or_else(|| find_gic(tree))
        .ok_or_else(|| {
            ax_err_type!(
                InvalidData,
                "guest FDT has no GIC interrupt-controller node"
            )
        })?;
    let cpu_region = match profile.cpu_region {
        GuestGicCpuRegion::CpuInterface(region) | GuestGicCpuRegion::Redistributors(region) => {
            region
        }
    };
    tree.inner_mut()
        .view_typed_mut(controller)
        .ok_or_else(|| ax_err_type!(InvalidData, "guest GIC node is missing"))?
        .set_regs(&[
            RegInfo::new(
                profile.distributor.base as u64,
                Some(profile.distributor.length as u64),
            ),
            RegInfo::new(cpu_region.base as u64, Some(cpu_region.length as u64)),
        ]);
    tree.set_property(controller, prop_string("compatible", &profile.compatible))?;
    tree.set_property(controller, prop_u32("#interrupt-cells", 3))?;
    if let Some(phandle) = profile.node_phandle {
        install_controller_phandle(tree, controller, phandle, "GIC")?;
    }
    Ok(())
}

fn install_plic_registers(tree: &mut FdtTree, profile: &GuestPlicProfile) -> AxVmResult {
    let controller = tree
        .inner()
        .get_by_path_id(&profile.node_path)
        .or_else(|| find_plic(tree))
        .ok_or_else(|| {
            ax_err_type!(
                InvalidData,
                "guest FDT has no PLIC interrupt-controller node"
            )
        })?;
    tree.inner_mut()
        .view_typed_mut(controller)
        .ok_or_else(|| ax_err_type!(InvalidData, "guest PLIC node is missing"))?
        .set_regs(&[RegInfo::new(
            profile.base as u64,
            Some(profile.length as u64),
        )]);
    tree.set_property(controller, prop_u32("#interrupt-cells", 1))?;
    if let Some(phandle) = profile.node_phandle {
        install_controller_phandle(tree, controller, phandle, "PLIC")?;
    }
    Ok(())
}

fn install_controller_phandle(
    tree: &mut FdtTree,
    controller: NodeId,
    phandle: u32,
    controller_name: &str,
) -> AxVmResult {
    if let Some(existing) = tree.inner().get_by_phandle(phandle.into())
        && existing.id() != controller
    {
        return Err(ax_err_type!(
            InvalidData,
            alloc::format!(
                "host {controller_name} phandle {phandle:#x} conflicts with another guest node"
            )
        ));
    }
    let old_phandle = tree
        .inner()
        .node(controller)
        .and_then(|node| {
            node.get_property("phandle")
                .or_else(|| node.get_property("linux,phandle"))
        })
        .and_then(Property::get_u32);
    if let Some(old_phandle) = old_phandle.filter(|old| *old != phandle) {
        let references = tree
            .inner()
            .iter_node_ids()
            .filter(|node_id| {
                tree.inner().node(*node_id).is_some_and(|node| {
                    ["interrupt-parent", "msi-parent"].into_iter().any(|name| {
                        node.get_property(name).and_then(Property::get_u32) == Some(old_phandle)
                    })
                })
            })
            .collect::<alloc::vec::Vec<_>>();
        for node_id in references {
            for name in ["interrupt-parent", "msi-parent"] {
                let matches = tree
                    .inner()
                    .node(node_id)
                    .and_then(|node| node.get_property(name))
                    .and_then(Property::get_u32)
                    == Some(old_phandle);
                if matches {
                    tree.set_property(node_id, prop_u32(name, phandle))?;
                }
            }
        }
    }
    tree.set_property(controller, prop_u32("phandle", phandle))?;
    tree.set_property(controller, prop_u32("linux,phandle", phandle))
}

fn prop_u32(name: &str, value: u32) -> Property {
    let mut property = Property::new(name, alloc::vec![]);
    property.set_u32_ls(&[value]);
    property
}

fn prop_string(name: &str, value: &str) -> Property {
    let mut property = Property::new(name, alloc::vec![]);
    property.set_string(value);
    property
}

fn find_gic(tree: &FdtTree) -> Option<NodeId> {
    tree.inner().iter_node_ids().find(|node_id| {
        tree.inner().node(*node_id).is_some_and(|node| {
            node.get_property("interrupt-controller").is_some()
                && node.compatibles().any(is_supported_gic)
        })
    })
}

fn is_supported_gic(compatible: &str) -> bool {
    matches!(
        compatible,
        "arm,gic-v3" | "arm,cortex-a15-gic" | "arm,gic-400"
    )
}

fn find_host_gic(fdt: &Fdt) -> Option<NodeId> {
    fdt.iter_node_ids().find(|node_id| {
        fdt.node(*node_id).is_some_and(|node| {
            node.get_property("interrupt-controller").is_some()
                && node.compatibles().any(is_supported_gic)
        })
    })
}

fn find_plic(tree: &FdtTree) -> Option<NodeId> {
    find_plic_in_fdt(tree.inner())
}

fn find_plic_in_fdt(fdt: &Fdt) -> Option<NodeId> {
    fdt.iter_node_ids().find(|node_id| {
        fdt.node(*node_id).is_some_and(|node| {
            node.get_property("interrupt-controller").is_some()
                && node
                    .compatibles()
                    .any(|compatible| matches!(compatible, "riscv,plic0" | "sifive,plic-1.0.0"))
        })
    })
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use fdt_edit::{Fdt, Node, Property};

    use super::*;

    #[test]
    fn replaces_host_gic_windows_with_virtual_machine_windows() {
        let mut tree = FdtTree::new();
        let root = tree.inner().root_id();
        let controller = tree.add_node(root, Node::new("intc@8000000"));
        let mut compatible = Property::new("compatible", vec![]);
        compatible.set_string("arm,gic-v3");
        tree.set_property(controller, compatible).unwrap();
        tree.set_property(controller, Property::new("interrupt-controller", vec![]))
            .unwrap();

        let profile = GuestGicProfile {
            compatible: "arm,gic-v3".into(),
            node_path: "/intc@8000000".into(),
            node_phandle: Some(1),
            distributor: GuestMmioRegion {
                base: 0x0800_0000,
                length: 0x1_0000,
            },
            cpu_region: GuestGicCpuRegion::Redistributors(GuestMmioRegion {
                base: 0x080a_0000,
                length: 0x2_0000,
            }),
        };
        install_gic_registers(&mut tree, &profile).unwrap();
        let bytes = tree.finish();
        let fdt = Fdt::from_bytes(&bytes).unwrap();
        let regs = fdt.get_by_path("/intc@8000000").unwrap().regs();

        assert_eq!(regs.len(), 2);
        assert_eq!(
            (regs[0].address, regs[0].size),
            (0x0800_0000, Some(0x1_0000))
        );
        assert_eq!(
            (regs[1].address, regs[1].size),
            (0x080a_0000, Some(0x2_0000))
        );
        let controller = fdt.get_by_path("/intc@8000000").unwrap();
        assert_eq!(
            controller
                .as_node()
                .get_property("linux,phandle")
                .unwrap()
                .get_u32(),
            Some(1)
        );
        assert_eq!(
            controller
                .as_node()
                .get_property("#interrupt-cells")
                .unwrap()
                .get_u32(),
            Some(3)
        );
    }

    #[test]
    fn resolves_host_gic_windows_and_phandle() {
        let mut tree = FdtTree::new();
        let root = tree.inner().root_id();
        tree.set_property(root, prop_u32("#address-cells", 2))
            .unwrap();
        tree.set_property(root, prop_u32("#size-cells", 2)).unwrap();
        let controller = tree.add_node(root, Node::new("interrupt-controller@fe600000"));
        let mut compatible = Property::new("compatible", vec![]);
        compatible.set_string("arm,gic-v3");
        tree.set_property(controller, compatible).unwrap();
        tree.set_property(controller, Property::new("interrupt-controller", vec![]))
            .unwrap();
        tree.set_property(controller, prop_u32("phandle", 1))
            .unwrap();
        tree.inner_mut()
            .view_typed_mut(controller)
            .unwrap()
            .set_regs(&[
                RegInfo::new(0xfe60_0000, Some(0x1_0000)),
                RegInfo::new(0xfe68_0000, Some(0x10_0000)),
            ]);
        let fdt = Fdt::from_bytes(&tree.finish()).unwrap();

        let profile = host_gic_profile(&fdt).unwrap().unwrap();

        assert_eq!(
            profile,
            GuestGicProfile {
                compatible: "arm,gic-v3".into(),
                node_path: "/interrupt-controller@fe600000".into(),
                node_phandle: Some(1),
                distributor: GuestMmioRegion {
                    base: 0xfe60_0000,
                    length: 0x1_0000,
                },
                cpu_region: GuestGicCpuRegion::Redistributors(GuestMmioRegion {
                    base: 0xfe68_0000,
                    length: 0x10_0000,
                }),
            }
        );
    }

    #[test]
    fn resolves_and_reuses_host_gicv2_windows() {
        let mut tree = FdtTree::new();
        let root = tree.inner().root_id();
        tree.set_property(root, prop_u32("#address-cells", 2))
            .unwrap();
        tree.set_property(root, prop_u32("#size-cells", 2)).unwrap();
        let controller = tree.add_node(root, Node::new("interrupt-controller@8000000"));
        let mut compatible = Property::new("compatible", vec![]);
        compatible.set_string("arm,cortex-a15-gic");
        tree.set_property(controller, compatible).unwrap();
        tree.set_property(controller, Property::new("interrupt-controller", vec![]))
            .unwrap();
        tree.set_property(controller, prop_u32("phandle", 1))
            .unwrap();
        tree.inner_mut()
            .view_typed_mut(controller)
            .unwrap()
            .set_regs(&[
                RegInfo::new(0x0800_0000, Some(0x1_0000)),
                RegInfo::new(0x0801_0000, Some(0x2_0000)),
            ]);
        let bytes = tree.finish();
        let fdt = Fdt::from_bytes(&bytes).unwrap();

        let profile = host_gic_profile(&fdt)
            .unwrap()
            .expect("host GICv2 must produce a machine interrupt profile");
        assert_eq!(profile.compatible, "arm,cortex-a15-gic");
        assert_eq!(
            profile.cpu_region,
            GuestGicCpuRegion::CpuInterface(GuestMmioRegion {
                base: 0x0801_0000,
                length: 0x2_0000,
            })
        );
        let mut guest = FdtTree::from_bytes(&bytes).unwrap();
        install_gic_registers(&mut guest, &profile).unwrap();
        let guest = Fdt::from_bytes(&guest.finish()).unwrap();
        let regs = guest
            .get_by_path("/interrupt-controller@8000000")
            .unwrap()
            .regs();

        assert_eq!(regs.len(), 2);
        assert_eq!(
            (regs[0].address, regs[0].size),
            (0x0800_0000, Some(0x1_0000))
        );
        assert_eq!(
            (regs[1].address, regs[1].size),
            (0x0801_0000, Some(0x2_0000))
        );
    }

    #[test]
    fn resolves_host_gic_maintenance_ppi() {
        let mut tree = FdtTree::new();
        let root = tree.inner().root_id();
        let controller = tree.add_node(root, Node::new("interrupt-controller@fe600000"));
        let mut compatible = Property::new("compatible", vec![]);
        compatible.set_string("arm,gic-v3");
        tree.set_property(controller, compatible).unwrap();
        tree.set_property(controller, Property::new("interrupt-controller", vec![]))
            .unwrap();
        tree.set_property(controller, prop_u32("#interrupt-cells", 3))
            .unwrap();
        let mut interrupts = Property::new("interrupts", vec![]);
        interrupts.set_u32_ls(&[1, 9, 4]);
        tree.set_property(controller, interrupts).unwrap();
        let fdt = Fdt::from_bytes(&tree.finish()).unwrap();

        assert_eq!(host_gic_maintenance_intid(&fdt).unwrap(), Some(25));
    }

    #[test]
    fn resolves_and_reuses_host_plic_window_and_phandle() {
        let mut tree = FdtTree::new();
        let root = tree.inner().root_id();
        tree.set_property(root, prop_u32("#address-cells", 2))
            .unwrap();
        tree.set_property(root, prop_u32("#size-cells", 2)).unwrap();
        let soc = tree.add_node(root, Node::new("soc"));
        tree.set_property(soc, prop_u32("#address-cells", 2))
            .unwrap();
        tree.set_property(soc, prop_u32("#size-cells", 2)).unwrap();
        let controller = tree.add_node(soc, Node::new("plic@d000000"));
        let mut compatible = Property::new("compatible", vec![]);
        compatible.set_string("riscv,plic0");
        tree.set_property(controller, compatible).unwrap();
        tree.set_property(controller, Property::new("interrupt-controller", vec![]))
            .unwrap();
        tree.set_property(controller, prop_u32("phandle", 9))
            .unwrap();
        tree.inner_mut()
            .view_typed_mut(controller)
            .unwrap()
            .set_regs(&[RegInfo::new(0x0d00_0000, Some(0x80_0000))]);
        let bytes = tree.finish();
        let fdt = Fdt::from_bytes(&bytes).unwrap();

        let profile = host_plic_profile(&fdt).unwrap().unwrap();
        assert_eq!(
            profile,
            GuestPlicProfile {
                node_path: "/soc/plic@d000000".into(),
                node_phandle: Some(9),
                base: 0x0d00_0000,
                length: 0x80_0000,
            }
        );

        let mut guest = FdtTree::from_bytes(&bytes).unwrap();
        install_plic_registers(&mut guest, &profile).unwrap();
        let guest = Fdt::from_bytes(&guest.finish()).unwrap();
        let controller = guest.get_by_path("/soc/plic@d000000").unwrap();
        assert_eq!(
            controller
                .as_node()
                .get_property("phandle")
                .unwrap()
                .get_u32(),
            Some(9)
        );
        let regs = controller.regs();
        assert_eq!(
            (regs[0].address, regs[0].size),
            (0x0d00_0000, Some(0x80_0000))
        );
        assert_eq!(
            controller
                .as_node()
                .get_property("#interrupt-cells")
                .unwrap()
                .get_u32(),
            Some(1)
        );
    }
}
