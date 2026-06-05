use dtb_file::*;
use fdt_edit::*;

#[test]
fn test_interrupt_parent_inheritance() {
    let mut fdt = Fdt::new();
    let root_id = fdt.root_id();

    fdt.node_mut(root_id).unwrap().set_property(Property::new(
        "interrupt-parent",
        0x10_u32.to_be_bytes().to_vec(),
    ));

    let soc_id = fdt.add_node(root_id, Node::new("soc"));
    fdt.add_node(soc_id, Node::new("uart@1000"));

    let mut timer = Node::new("timer@2000");
    timer.set_property(Property::new(
        "interrupt-parent",
        0x20_u32.to_be_bytes().to_vec(),
    ));
    fdt.add_node(soc_id, timer);

    let soc = fdt.get_by_path("/soc").unwrap();
    assert_eq!(soc.as_node().interrupt_parent(), None);
    assert_eq!(soc.interrupt_parent(), Some(Phandle::from(0x10)));

    let uart = fdt.get_by_path("/soc/uart@1000").unwrap();
    assert_eq!(uart.as_node().interrupt_parent(), None);
    assert_eq!(uart.interrupt_parent(), Some(Phandle::from(0x10)));

    let timer = fdt.get_by_path("/soc/timer@2000").unwrap();
    assert_eq!(
        timer.as_node().interrupt_parent(),
        Some(Phandle::from(0x20))
    );
    assert_eq!(timer.interrupt_parent(), Some(Phandle::from(0x20)));
}

#[test]
fn test_interrupt_parent_inheritance_orangepi5plus() {
    let raw_data = fdt_orangepi_5plus();
    let fdt = Fdt::from_bytes(&raw_data).unwrap();

    let root = fdt.get_by_path("/").unwrap();
    assert_eq!(root.as_node().interrupt_parent(), Some(Phandle::from(0x01)));
    assert_eq!(root.interrupt_parent(), Some(Phandle::from(0x01)));

    let mmc = fdt.get_by_path("/mmc@fe2e0000").unwrap();
    assert_eq!(mmc.as_node().interrupt_parent(), None);
    assert_eq!(mmc.interrupt_parent(), Some(Phandle::from(0x01)));

    let irq_parent = fdt.get_by_phandle(Phandle::from(0x01)).unwrap();
    assert_eq!(irq_parent.path(), "/interrupt-controller@fe600000");
}

#[test]
fn test_iter_nodes() {
    let raw_data = fdt_phytium();
    let fdt = Fdt::from_bytes(&raw_data).unwrap();
    let mut count = 0;
    for view in fdt.all_nodes() {
        println!("{:?} path={}", view.as_node(), view.path());
        count += 1;
    }
    assert!(count > 0, "should have at least one node");
    assert_eq!(count, fdt.node_count());
}

#[test]
fn test_node_classify() {
    let raw_data = fdt_phytium();
    let fdt = Fdt::from_bytes(&raw_data).unwrap();

    let mut memory_count = 0;
    let mut intc_count = 0;
    let mut generic_count = 0;

    for view in fdt.all_nodes() {
        match view {
            NodeType::Clock(clock) => {
                println!(
                    "Clock node: {} #clock-cells={}",
                    clock.path(),
                    clock.clock_cells()
                );
            }
            NodeType::Pci(pci) => {
                println!(
                    "PCI node: {} #interrupt-cells={}",
                    pci.path(),
                    pci.interrupt_cells()
                );
            }
            NodeType::Memory(mem) => {
                memory_count += 1;
                let regions = mem.regions();
                println!(
                    "Memory node: {} regions={} total_size={:#x}",
                    mem.path(),
                    regions.len(),
                    mem.total_size()
                );
            }
            NodeType::InterruptController(intc) => {
                intc_count += 1;
                println!(
                    "IntC node: {} #interrupt-cells={:?}",
                    intc.path(),
                    intc.interrupt_cells()
                );
            }
            NodeType::Generic(g) => {
                generic_count += 1;
                let _ = g.path();
            }
        }
    }

    println!(
        "memory={}, intc={}, generic={}",
        memory_count, intc_count, generic_count
    );
    assert!(memory_count > 0, "phytium DTB should have memory nodes");
    assert!(intc_count > 0, "phytium DTB should have intc nodes");
    assert!(generic_count > 0, "phytium DTB should have generic nodes");
}

#[test]
fn test_path_lookup() {
    let raw_data = fdt_phytium();
    let fdt = Fdt::from_bytes(&raw_data).unwrap();

    // Root should always be found
    let root = fdt.get_by_path("/").unwrap();
    assert_eq!(root.id(), fdt.root_id());

    // Check path round-trip: for every node, path_of(id) should resolve back
    for id in fdt.iter_node_ids() {
        let path = fdt.path_of(id);
        let found = fdt.get_by_path_id(&path);
        assert_eq!(
            found,
            Some(id),
            "path_of({}) = {:?} did not resolve back",
            id,
            path
        );
    }

    // Verify get_by_path returns correct NodeType classification
    for view in fdt.all_nodes() {
        let path = view.path();
        let typed = fdt.get_by_path(&path).unwrap();
        assert_eq!(typed.id(), view.id());
    }
}

#[test]
fn test_display_nodes() {
    let raw_data = fdt_phytium();
    let fdt = Fdt::from_bytes(&raw_data).unwrap();
    for view in fdt.all_nodes() {
        println!("{}", view);
    }
}
