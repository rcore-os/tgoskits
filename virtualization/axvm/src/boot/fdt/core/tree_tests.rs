use std::{vec, vec::Vec};

use fdt_edit::{Fdt, Node, Property};
use fdt_raw::{MemoryReservation, RegInfo};

use super::tree::{FdtTree, GuestMemorySpec, host_fdt_bytes_from_ptr};

fn prop_u32(name: &str, value: u32) -> Property {
    let mut prop = Property::new(name, vec![]);
    prop.set_u32_ls(&[value]);
    prop
}

fn prop_str(name: &str, value: &str) -> Property {
    let mut prop = Property::new(name, vec![]);
    prop.set_string(value);
    prop
}

fn sample_dtb() -> Vec<u8> {
    let mut fdt = Fdt::new();
    let root = fdt.root_id();
    fdt.node_mut(root)
        .unwrap()
        .set_property(prop_u32("#address-cells", 2));
    fdt.node_mut(root)
        .unwrap()
        .set_property(prop_u32("#size-cells", 2));

    let mut chosen = Node::new("chosen");
    chosen.set_property(prop_str(
        "bootargs",
        "root=/dev/vda ro console=ttyS0 rootwait",
    ));
    chosen.set_property(prop_u32("linux,initrd-start", 0x4000));
    chosen.set_property(prop_u32("linux,initrd-end", 0x8000));
    fdt.add_node(root, chosen);

    let memory = fdt.add_node(root, Node::new("memory@40000000"));
    fdt.node_mut(memory)
        .unwrap()
        .set_property(prop_str("device_type", "memory"));
    fdt.view_typed_mut(memory)
        .unwrap()
        .set_regs(&[RegInfo::new(0x4000_0000, Some(0x1000_0000))]);

    fdt.encode().as_ref().to_vec()
}

#[test]
fn tree_rebuilds_memory_nodes_from_guest_regions() {
    let mut tree = FdtTree::from_bytes(&sample_dtb()).unwrap();

    tree.rebuild_memory_nodes(&[
        GuestMemorySpec::new(0x8000_0000, 0x0200_0000),
        GuestMemorySpec::new(0x9000_0000, 0x0100_0000),
    ])
    .unwrap();
    let bytes = tree.finish();
    let reparsed = Fdt::from_bytes(&bytes).unwrap();
    let memory_paths = reparsed
        .iter_node_ids()
        .map(|id| reparsed.path_of(id))
        .filter(|path| path.starts_with("/memory"))
        .collect::<std::vec::Vec<_>>();

    assert_eq!(memory_paths, ["/memory@80000000", "/memory@90000000"]);
    let first = reparsed.get_by_path("/memory@80000000").unwrap();
    assert_eq!(first.regs()[0].address, 0x8000_0000);
    assert_eq!(first.regs()[0].size, Some(0x0200_0000));
}

#[test]
fn tree_patches_chosen_bootargs_and_initrd() {
    let mut tree = FdtTree::from_bytes(&sample_dtb()).unwrap();

    tree.patch_chosen(Some((0xa000_0000, 0x1234)), None)
        .unwrap();
    let bytes = tree.finish();
    let reparsed = Fdt::from_bytes(&bytes).unwrap();
    let chosen = reparsed.get_by_path("/chosen").unwrap();
    let chosen_node = chosen.as_node();

    assert_eq!(
        chosen_node.get_property("bootargs").unwrap().as_str(),
        Some("root=/dev/vda rw console=ttyS0 rootwait fsck.repair=yes")
    );
    assert_eq!(
        chosen_node
            .get_property("linux,initrd-start")
            .unwrap()
            .get_u64(),
        Some(0xa000_0000)
    );
    assert_eq!(
        chosen_node
            .get_property("linux,initrd-end")
            .unwrap()
            .get_u64(),
        Some(0xa000_1234)
    );
}

#[test]
fn tree_removes_stale_initrd_when_no_ramdisk_is_present() {
    let mut tree = FdtTree::from_bytes(&sample_dtb()).unwrap();

    tree.patch_chosen(None, None).unwrap();
    let bytes = tree.finish();
    let reparsed = Fdt::from_bytes(&bytes).unwrap();
    let chosen = reparsed.get_by_path("/chosen").unwrap();
    let chosen_node = chosen.as_node();

    assert!(chosen_node.get_property("linux,initrd-start").is_none());
    assert!(chosen_node.get_property("linux,initrd-end").is_none());
}

#[test]
fn explicit_cmdline_replaces_host_bootargs() {
    let mut tree = FdtTree::from_bytes(&sample_dtb()).unwrap();

    tree.patch_chosen(
        None,
        Some(
            "root=/dev/mmcblk0p2 rw console=ttyAMA2,1500000 earlycon=pl011,mmio32,0xfeb50000 \
             rootwait",
        ),
    )
    .unwrap();
    let bytes = tree.finish();
    let reparsed = Fdt::from_bytes(&bytes).unwrap();
    let chosen = reparsed.get_by_path("/chosen").unwrap();

    assert_eq!(
        chosen.as_node().get_property("bootargs").unwrap().as_str(),
        Some(
            "root=/dev/mmcblk0p2 rw console=ttyAMA2,1500000 earlycon=pl011,mmio32,0xfeb50000 \
             rootwait fsck.repair=yes"
        )
    );
}

#[test]
fn host_fdt_pointer_rejects_null() {
    assert!(host_fdt_bytes_from_ptr(std::ptr::null()).is_none());
}

#[test]
fn phandle_validation_rejects_reserved_and_conflicting_values() {
    let mut tree = FdtTree::new();
    let root = tree.inner().root_id();
    let reserved = tree.add_node(root, Node::new("reserved"));
    tree.set_property(reserved, prop_u32("phandle", u32::MAX))
        .unwrap();
    assert!(tree.allocate_phandle().is_err());

    tree.set_property(reserved, prop_u32("phandle", 1)).unwrap();
    tree.set_property(reserved, prop_u32("linux,phandle", 2))
        .unwrap();
    assert!(tree.allocate_phandle().is_err());
    tree.set_property(reserved, prop_u32("linux,phandle", 1))
        .unwrap();
    let duplicate = tree.ensure_path("/duplicate").unwrap();
    tree.set_property(duplicate, prop_u32("phandle", 1))
        .unwrap();
    assert!(tree.validate_phandles().is_err());
    tree.set_property(duplicate, Property::new("phandle", vec![0; 8]))
        .unwrap();
    assert!(tree.validate_phandles().is_err());
}

#[test]
fn tree_copies_subtree_and_exposes_mutable_inner_tree() {
    let mut source = Fdt::new();
    let source_root = source.root_id();
    let bus = source.add_node(source_root, Node::new("soc"));
    source
        .node_mut(bus)
        .unwrap()
        .set_property(prop_str("compatible", "simple-bus"));
    let uart = source.add_node(bus, Node::new("serial@1000"));
    source
        .node_mut(uart)
        .unwrap()
        .set_property(prop_str("status", "okay"));

    let mut dest = FdtTree::new();
    let copied = dest
        .copy_subtree_from(&source, bus, dest.inner().root_id(), false)
        .unwrap();
    dest.inner_mut()
        .node_mut(copied)
        .unwrap()
        .set_property(prop_str("dma-coherent", "true"));

    let bytes = dest.finish();
    let reparsed = Fdt::from_bytes(&bytes).unwrap();
    let copied_bus = reparsed.get_by_path("/soc").unwrap().as_node();
    let copied_uart = reparsed.get_by_path("/soc/serial@1000").unwrap().as_node();

    assert_eq!(
        copied_bus.get_property("compatible").unwrap().as_str(),
        Some("simple-bus")
    );
    assert_eq!(
        copied_bus.get_property("dma-coherent").unwrap().as_str(),
        Some("true")
    );
    assert_eq!(
        copied_uart.get_property("status").unwrap().as_str(),
        Some("okay")
    );
}

#[test]
fn finish_drops_host_header_state_from_guest_dtb() {
    let mut source = Fdt::new();
    source.boot_cpuid_phys = 0x100;
    source.memory_reservations.push(MemoryReservation {
        address: 0x8000_0000,
        size: 0x1000,
    });

    let tree = FdtTree::clone_filtered(&source, |_, _, _| true).unwrap();
    let bytes = tree.finish();
    let reparsed = Fdt::from_bytes(&bytes).unwrap();

    assert_eq!(reparsed.boot_cpuid_phys, 0);
    assert!(reparsed.memory_reservations.is_empty());
}

#[test]
fn clone_filtered_preserves_root_sibling_order() {
    let mut source = Fdt::new();
    let root = source.root_id();
    source.add_node(root, Node::new("timer"));
    source.add_node(root, Node::new("timer@feae0000"));
    source.add_node(root, Node::new("interrupt-controller@fe600000"));

    let tree = FdtTree::clone_filtered(&source, |_, _, _| true).unwrap();
    let bytes = tree.finish();
    let reparsed = Fdt::from_bytes(&bytes).unwrap();
    let root_node = reparsed.node(reparsed.root_id()).unwrap();
    let child_names = root_node
        .children()
        .iter()
        .map(|id| reparsed.node(*id).unwrap().name())
        .collect::<Vec<_>>();

    assert_eq!(
        child_names,
        ["timer", "timer@feae0000", "interrupt-controller@fe600000"]
    );
}

#[test]
fn clone_filtered_drops_guest_cpu_power_management_props() {
    let mut source = Fdt::new();
    let root = source.root_id();
    source
        .node_mut(root)
        .unwrap()
        .set_property(prop_str("compatible", "rockchip,rk3568-firefly-roc-pc-se"));

    let cpus = source.add_node(root, Node::new("cpus"));
    source
        .node_mut(cpus)
        .unwrap()
        .set_property(prop_u32("#address-cells", 2));
    source
        .node_mut(cpus)
        .unwrap()
        .set_property(prop_u32("#size-cells", 0));

    let cpu = source.add_node(cpus, Node::new("cpu@0"));
    let cpu_node = source.node_mut(cpu).unwrap();
    cpu_node.set_property(prop_str("device_type", "cpu"));
    cpu_node.set_property(prop_str("enable-method", "psci"));
    cpu_node.set_property(prop_u32("operating-points-v2", 3));
    cpu_node.set_property(prop_u32("#cooling-cells", 2));
    cpu_node.set_property(prop_u32("dynamic-power-coefficient", 0xbb));
    cpu_node.set_property(prop_u32("cpu-supply", 5));

    let tree = FdtTree::clone_filtered(&source, |_, _, _| true).unwrap();
    let bytes = tree.finish();
    let reparsed = Fdt::from_bytes(&bytes).unwrap();
    let cpu_node = reparsed.get_by_path("/cpus/cpu@0").unwrap().as_node();

    assert_eq!(
        cpu_node.get_property("enable-method").unwrap().as_str(),
        Some("psci")
    );
    assert!(cpu_node.get_property("operating-points-v2").is_none());
    assert!(cpu_node.get_property("#cooling-cells").is_none());
    assert!(cpu_node.get_property("dynamic-power-coefficient").is_none());
    assert!(cpu_node.get_property("cpu-supply").is_none());
}

#[test]
fn clone_filtered_preserves_guest_cpu_power_management_props_on_rk3588() {
    let mut source = Fdt::new();
    let root = source.root_id();
    source
        .node_mut(root)
        .unwrap()
        .set_property(prop_str("compatible", "rockchip,rk3588-orangepi-5-plus"));

    let cpus = source.add_node(root, Node::new("cpus"));
    let cpu = source.add_node(cpus, Node::new("cpu@0"));
    let cpu_node = source.node_mut(cpu).unwrap();
    cpu_node.set_property(prop_u32("operating-points-v2", 3));
    cpu_node.set_property(prop_u32("#cooling-cells", 2));
    cpu_node.set_property(prop_u32("dynamic-power-coefficient", 0xbb));
    cpu_node.set_property(prop_u32("cpu-supply", 5));

    let tree = FdtTree::clone_filtered(&source, |_, _, _| true).unwrap();
    let bytes = tree.finish();
    let reparsed = Fdt::from_bytes(&bytes).unwrap();
    let cpu_node = reparsed.get_by_path("/cpus/cpu@0").unwrap().as_node();

    assert_eq!(
        cpu_node
            .get_property("operating-points-v2")
            .unwrap()
            .get_u32(),
        Some(3)
    );
    assert_eq!(
        cpu_node.get_property("#cooling-cells").unwrap().get_u32(),
        Some(2)
    );
    assert_eq!(
        cpu_node
            .get_property("dynamic-power-coefficient")
            .unwrap()
            .get_u32(),
        Some(0xbb)
    );
    assert_eq!(
        cpu_node.get_property("cpu-supply").unwrap().get_u32(),
        Some(5)
    );
}

#[test]
fn subtree_import_rebinds_source_references_without_touching_guest_numbers() {
    let mut source = FdtTree::new();
    let bus = source.ensure_path("/imported").unwrap();
    let provider = source.ensure_path("/imported/clock").unwrap();
    source
        .set_property(provider, prop_u32("phandle", 1))
        .unwrap();
    source
        .set_property(provider, prop_u32("#clock-cells", 1))
        .unwrap();
    let consumer = source.ensure_path("/imported/device").unwrap();
    let mut clocks = Property::new("clocks", vec![]);
    clocks.set_u32_ls(&[1, 1]);
    source.set_property(consumer, clocks.clone()).unwrap();
    for name in ["#iommu-cells", "#interrupt-cells"] {
        source.set_property(provider, prop_u32(name, 1)).unwrap();
    }
    source
        .set_property(provider, prop_u32("#address-cells", 0))
        .unwrap();
    source
        .set_property(consumer, prop_u32("#address-cells", 0))
        .unwrap();
    source
        .set_property(consumer, prop_u32("#interrupt-cells", 1))
        .unwrap();
    for (name, cells) in [
        ("iommus", &[1, 1][..]),
        ("interrupts-extended", &[1, 1][..]),
        ("interrupt-map", &[1, 1, 1][..]),
        ("msi-map", &[1, 1, 1, 1][..]),
    ] {
        let mut property = Property::new(name, vec![]);
        property.set_u32_ls(cells);
        source.set_property(consumer, property).unwrap();
    }

    let mut guest = FdtTree::new();
    let existing = guest.ensure_path("/existing").unwrap();
    guest
        .set_property(existing, prop_u32("phandle", 1))
        .unwrap();
    guest.set_property(existing, clocks).unwrap();
    // A high occupied handle must not hide free values below it.
    let last = guest.ensure_path("/last").unwrap();
    guest
        .set_property(last, prop_u32("phandle", u32::MAX - 1))
        .unwrap();

    guest
        .copy_subtree_from(source.inner(), bus, guest.inner().root_id(), false)
        .unwrap();

    let bytes = guest.finish();
    let fdt = Fdt::from_bytes(&bytes).unwrap();
    let imported = fdt.get_by_path("/imported/clock").unwrap();
    let handle = imported
        .as_node()
        .get_property("phandle")
        .unwrap()
        .get_u32()
        .unwrap();
    assert!(handle > 1 && handle < u32::MAX - 1);
    let reference = fdt.get_by_path("/imported/device").unwrap();
    assert_eq!(
        reference
            .as_node()
            .get_property("clocks")
            .unwrap()
            .get_u32_iter()
            .collect::<Vec<_>>(),
        [handle, 1]
    );
    assert_eq!(
        fdt.get_by_path("/existing")
            .unwrap()
            .as_node()
            .get_property("clocks")
            .unwrap()
            .get_u32_iter()
            .collect::<Vec<_>>(),
        [1, 1]
    );
    for (name, expected) in [
        ("iommus", vec![handle, 1]),
        ("interrupts-extended", vec![handle, 1]),
        ("interrupt-map", vec![1, handle, 1]),
        ("msi-map", vec![1, handle, 1, 1]),
    ] {
        assert_eq!(
            reference
                .as_node()
                .get_property(name)
                .unwrap()
                .get_u32_iter()
                .collect::<Vec<_>>(),
            expected,
            "{name}"
        );
    }
    assert_eq!(source.node_phandle(provider).unwrap(), Some(1));
}

#[test]
fn subtree_import_rejects_unbound_dependencies_without_changing_destination() {
    let mut source = FdtTree::new();
    let provider = source.ensure_path("/clock").unwrap();
    source
        .set_property(provider, prop_u32("phandle", 7))
        .unwrap();
    source
        .set_property(provider, prop_u32("#clock-cells", 0))
        .unwrap();
    let device = source.ensure_path("/device").unwrap();
    source.set_property(device, prop_u32("clocks", 7)).unwrap();
    let mut guest = FdtTree::new();
    let unrelated = guest.ensure_path("/unrelated").unwrap();
    guest
        .set_property(unrelated, prop_u32("phandle", 7))
        .unwrap();
    let before = guest.inner().encode().as_ref().to_vec();
    let error = guest
        .copy_subtree_from(source.inner(), device, guest.inner().root_id(), false)
        .unwrap_err();
    assert!(error.to_string().contains("outside the imported subtree"));
    assert_eq!(guest.inner().encode().as_ref(), before);
}
