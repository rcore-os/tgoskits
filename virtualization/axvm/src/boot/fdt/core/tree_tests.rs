use alloc::{vec, vec::Vec};

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
        .collect::<alloc::vec::Vec<_>>();

    assert_eq!(memory_paths, ["/memory@80000000", "/memory@90000000"]);
    let first = reparsed.get_by_path("/memory@80000000").unwrap();
    assert_eq!(first.regs()[0].address, 0x8000_0000);
    assert_eq!(first.regs()[0].size, Some(0x0200_0000));
}

#[test]
fn tree_patches_chosen_bootargs_and_initrd() {
    let mut tree = FdtTree::from_bytes(&sample_dtb()).unwrap();

    tree.patch_chosen(Some((0xa000_0000, 0x1234))).unwrap();
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

    tree.patch_chosen(None).unwrap();
    let bytes = tree.finish();
    let reparsed = Fdt::from_bytes(&bytes).unwrap();
    let chosen = reparsed.get_by_path("/chosen").unwrap();
    let chosen_node = chosen.as_node();

    assert!(chosen_node.get_property("linux,initrd-start").is_none());
    assert!(chosen_node.get_property("linux,initrd-end").is_none());
}

#[test]
fn host_fdt_pointer_rejects_null() {
    assert!(host_fdt_bytes_from_ptr(core::ptr::null()).is_none());
}

#[test]
fn tree_copies_subtree_and_updates_a_property() {
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
    dest.set_property(copied, prop_str("dma-coherent", "true"))
        .unwrap();

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
fn tree_removes_a_subtree_and_selected_node_properties() {
    let mut source = Fdt::new();
    let root = source.root_id();
    let cpus = source.add_node(root, Node::new("cpus"));
    source
        .node_mut(cpus)
        .unwrap()
        .set_property(prop_u32("#address-cells", 1));
    source
        .node_mut(cpus)
        .unwrap()
        .set_property(prop_u32("riscv,cbop-block-size", 64));
    source.add_node(cpus, Node::new("cpu@0"));
    source.add_node(cpus, Node::new("cpu@1"));
    let mut tree = FdtTree::from_fdt(source);

    assert!(tree.remove_subtree("/cpus/cpu@1").is_some());
    tree.edit_node(cpus, |node| {
        node.remove_property("riscv,cbop-block-size");
    })
    .unwrap();

    let bytes = tree.finish();
    let reparsed = Fdt::from_bytes(&bytes).unwrap();
    let cpus = reparsed.get_by_path("/cpus").unwrap().as_node();
    assert!(reparsed.get_by_path("/cpus/cpu@0").is_some());
    assert!(reparsed.get_by_path("/cpus/cpu@1").is_none());
    assert!(cpus.get_property("#address-cells").is_some());
    assert!(cpus.get_property("riscv,cbop-block-size").is_none());
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
