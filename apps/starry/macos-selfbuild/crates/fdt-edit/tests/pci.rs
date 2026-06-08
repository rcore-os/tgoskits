//! PCI node view tests.

use std::sync::Once;

use dtb_file::*;
use fdt_edit::{Fdt, NodeType, PciRange, PciSpace};

fn init_logging() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        let _ = env_logger::builder()
            .is_test(true)
            .filter_level(log::LevelFilter::Trace)
            .try_init();
    });
}

#[test]
fn test_pci_node_detection() {
    let raw_data = fdt_phytium();
    let fdt = Fdt::from_bytes(&raw_data).unwrap();

    let mut pci_count = 0;
    for node in fdt.all_nodes() {
        if let NodeType::Pci(pci) = node {
            pci_count += 1;
            println!(
                "PCI node: {} #interrupt-cells={}",
                pci.path(),
                pci.interrupt_cells()
            );
        }
    }

    println!("Total PCI nodes: {}", pci_count);
    // 飞腾 DTB 应该有 PCI/PCIe 节点
    assert!(pci_count > 0, "phytium DTB should have PCI nodes");
}

#[test]
fn test_pci_ranges() {
    let raw_data = fdt_phytium();
    let fdt = Fdt::from_bytes(&raw_data).unwrap();

    for node in fdt.all_nodes() {
        if let NodeType::Pci(pci) = node
            && let Some(ranges) = pci.ranges()
        {
            println!("PCI {} has {} ranges:", pci.path(), ranges.len());

            for range in &ranges {
                let space_name = match range.space {
                    PciSpace::IO => "IO",
                    PciSpace::Memory32 => "Mem32",
                    PciSpace::Memory64 => "Mem64",
                };

                println!(
                    "  {}: bus={:#x} cpu={:#x} size={:#x} prefetch={}",
                    space_name,
                    range.bus_address,
                    range.cpu_address,
                    range.size,
                    range.prefetchable
                );
            }

            assert!(!ranges.is_empty(), "PCI node should have ranges");
        }
    }
}

#[test]
fn test_pci_bus_range() {
    let raw_data = fdt_phytium();
    let fdt = Fdt::from_bytes(&raw_data).unwrap();

    for node in fdt.all_nodes() {
        if let NodeType::Pci(pci) = node
            && let Some(bus_range) = pci.bus_range()
        {
            println!(
                "PCI {} bus-range: {}..{}",
                pci.path(),
                bus_range.start,
                bus_range.end
            );
        }
    }
}

#[test]
fn test_pci_interrupt_map_mask() {
    let raw_data = fdt_phytium();
    let fdt = Fdt::from_bytes(&raw_data).unwrap();

    for node in fdt.all_nodes() {
        if let NodeType::Pci(pci) = node
            && let Some(mask) = pci.interrupt_map_mask()
        {
            println!("PCI {} interrupt-map-mask: {:?}", pci.path(), mask);
            assert!(!mask.is_empty(), "interrupt-map-mask should not be empty");
        }
    }
}

#[test]
fn test_pci2() {
    let raw = fdt_phytium();
    let fdt = Fdt::from_bytes(&raw).unwrap();
    let node = fdt
        .find_compatible(&["pci-host-ecam-generic"])
        .into_iter()
        .next()
        .unwrap();

    let NodeType::Pci(pci) = node else {
        panic!("Not a PCI node");
    };

    let want = [
        PciRange {
            space: PciSpace::IO,
            bus_address: 0x0,
            cpu_address: 0x50000000,
            size: 0xf00000,
            prefetchable: false,
        },
        PciRange {
            space: PciSpace::Memory32,
            bus_address: 0x58000000,
            cpu_address: 0x58000000,
            size: 0x28000000,
            prefetchable: false,
        },
        PciRange {
            space: PciSpace::Memory64,
            bus_address: 0x1000000000,
            cpu_address: 0x1000000000,
            size: 0x1000000000,
            prefetchable: false,
        },
    ];

    for (i, range) in pci.ranges().unwrap().iter().enumerate() {
        assert_eq!(*range, want[i]);
        println!("{range:#x?}");
    }
}

#[test]
fn test_pci_irq_map() {
    let raw = fdt_phytium();
    let fdt = Fdt::from_bytes(&raw).unwrap();
    let node_ref = fdt
        .find_compatible(&["pci-host-ecam-generic"])
        .into_iter()
        .next()
        .unwrap();

    let NodeType::Pci(pci) = node_ref else {
        panic!("Not a PCI node");
    };

    let irq = pci.child_interrupts(0, 0, 0, 4).unwrap();

    assert!(!irq.irqs.is_empty());
}

#[test]
fn test_pci_irq_map2() {
    init_logging();

    let raw = fdt_qemu();
    let fdt = Fdt::from_bytes(&raw).unwrap();
    let node_ref = fdt
        .find_compatible(&["pci-host-ecam-generic"])
        .into_iter()
        .next()
        .unwrap();

    let NodeType::Pci(pci) = node_ref else {
        panic!("Not a PCI node");
    };

    let irq = pci.child_interrupts(0, 2, 0, 1).unwrap();

    let want = [0, 5, 4];

    for (got, want) in irq.irqs.iter().zip(want.iter()) {
        assert_eq!(*got, *want);
    }
}
