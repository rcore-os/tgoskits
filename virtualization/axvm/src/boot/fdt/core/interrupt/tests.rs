use std::vec;

use axdevice_base::ItsId;
use fdt_edit::{Fdt, Node, Property};
use fdt_raw::RegInfo;

use super::{
    super::tree::FdtTree,
    gic, host_gic_maintenance_intid, host_gic_profile, host_plic_profile,
    phandle::{prop_string, prop_u32, prop_u64},
    plic,
};
use crate::machine::*;

#[test]
fn replaces_host_gic_windows_with_virtual_machine_windows() {
    let mut tree = FdtTree::new();
    let root = tree.inner().root_id();
    let controller = tree.add_node(root, Node::new("intc@8000000"));
    tree.set_property(controller, prop_string("compatible", "arm,gic-v3"))
        .unwrap();
    tree.set_property(controller, Property::new("interrupt-controller", vec![]))
        .unwrap();
    let its = tree.add_node(root, Node::new("its@8080000"));
    tree.set_property(its, prop_string("compatible", "arm,gic-v3-its"))
        .unwrap();
    tree.set_property(its, Property::new("msi-controller", vec![]))
        .unwrap();
    tree.set_property(its, prop_u32("phandle", 7)).unwrap();
    let device = tree.add_node(root, Node::new("virtio@a000000"));
    tree.set_property(device, prop_u32("msi-parent", 7))
        .unwrap();

    let profile = GuestGicProfile {
        compatible: "arm,gic-v3".into(),
        node_path: "/intc@8000000".into(),
        node_phandle: Some(1),
        distributor: GuestMmioRegion {
            base: 0x0800_0000,
            length: 0x1_0000,
        },
        cpu_region: GuestGicCpuRegion::Redistributors(GuestGicRedistributorProfile {
            regions: vec![
                GuestMmioRegion {
                    base: 0x080a_0000,
                    length: 0x4_0000,
                },
                GuestMmioRegion {
                    base: 0x0810_0000,
                    length: 0x4_0000,
                },
            ],
            stride: 0x4_0000,
        }),
        its: vec![GuestItsProfile {
            id: ItsId::new(0),
            node_path: "/its@8080000".into(),
            node_phandle: Some(4),
            registers: GuestMmioRegion {
                base: 0x0808_0000,
                length: 0x2_0000,
            },
        }],
    };
    gic::install_registers(&mut tree, &profile).unwrap();
    let bytes = tree.finish();
    let fdt = Fdt::from_bytes(&bytes).unwrap();
    let regs = fdt.get_by_path("/intc@8000000").unwrap().regs();

    assert_eq!(regs.len(), 3);
    assert_eq!(
        (regs[0].address, regs[0].size),
        (0x0800_0000, Some(0x1_0000))
    );
    assert_eq!(
        (regs[1].address, regs[1].size),
        (0x080a_0000, Some(0x4_0000))
    );
    assert_eq!(
        (regs[2].address, regs[2].size),
        (0x0810_0000, Some(0x4_0000))
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
    assert_eq!(
        controller
            .as_node()
            .get_property("redistributor-stride")
            .unwrap()
            .get_u64(),
        Some(0x4_0000)
    );
    let its = fdt.get_by_path("/its@8080000").unwrap();
    assert_eq!(its.regs()[0].address, 0x0808_0000);
    assert_eq!(
        its.as_node().get_property("phandle").unwrap().get_u32(),
        Some(4)
    );
    assert_eq!(
        fdt.get_by_path("/virtio@a000000")
            .unwrap()
            .as_node()
            .get_property("msi-parent")
            .unwrap()
            .get_u32(),
        Some(4)
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
    tree.set_property(controller, prop_string("compatible", "arm,gic-v3"))
        .unwrap();
    tree.set_property(controller, Property::new("interrupt-controller", vec![]))
        .unwrap();
    tree.set_property(controller, prop_u32("phandle", 1))
        .unwrap();
    tree.set_property(controller, prop_u32("#redistributor-regions", 2))
        .unwrap();
    tree.inner_mut()
        .view_typed_mut(controller)
        .unwrap()
        .set_regs(&[
            RegInfo::new(0xfe60_0000, Some(0x1_0000)),
            RegInfo::new(0xfe68_0000, Some(0x4_0000)),
            RegInfo::new(0xfe80_0000, Some(0x4_0000)),
            RegInfo::new(0xfe61_0000, Some(0x1_0000)),
            RegInfo::new(0xfe62_0000, Some(0x1_0000)),
            RegInfo::new(0xfe63_0000, Some(0x1_0000)),
        ]);
    tree.set_property(controller, prop_u64("redistributor-stride", 0x4_0000))
        .unwrap();
    let its = tree.add_node(root, Node::new("its@fe640000"));
    tree.set_property(its, prop_string("compatible", "arm,gic-v3-its"))
        .unwrap();
    tree.set_property(its, Property::new("msi-controller", vec![]))
        .unwrap();
    tree.set_property(its, prop_u32("phandle", 4)).unwrap();
    tree.inner_mut()
        .view_typed_mut(its)
        .unwrap()
        .set_regs(&[RegInfo::new(0xfe64_0000, Some(0x2_0000))]);
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
            cpu_region: GuestGicCpuRegion::Redistributors(GuestGicRedistributorProfile {
                regions: vec![
                    GuestMmioRegion {
                        base: 0xfe68_0000,
                        length: 0x4_0000,
                    },
                    GuestMmioRegion {
                        base: 0xfe80_0000,
                        length: 0x4_0000,
                    },
                ],
                stride: 0x4_0000,
            }),
            its: vec![GuestItsProfile {
                id: ItsId::new(0),
                node_path: "/its@fe640000".into(),
                node_phandle: Some(4),
                registers: GuestMmioRegion {
                    base: 0xfe64_0000,
                    length: 0x2_0000,
                },
            }],
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
    tree.set_property(controller, prop_string("compatible", "arm,cortex-a15-gic"))
        .unwrap();
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
    assert!(profile.its.is_empty());
    let mut guest = FdtTree::from_bytes(&bytes).unwrap();
    gic::install_registers(&mut guest, &profile).unwrap();
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
    tree.set_property(controller, prop_string("compatible", "arm,gic-v3"))
        .unwrap();
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
    tree.set_property(controller, prop_string("compatible", "riscv,plic0"))
        .unwrap();
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
    plic::install_registers(&mut guest, &profile).unwrap();
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
