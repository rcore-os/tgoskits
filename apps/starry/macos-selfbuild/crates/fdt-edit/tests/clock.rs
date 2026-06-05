//! Clock node view tests.

use dtb_file::*;
use fdt_edit::{ClockRef, Fdt, NodeType};

#[test]
fn test_clock_node_detection() {
    let raw_data = fdt_phytium();
    let fdt = Fdt::from_bytes(&raw_data).unwrap();

    let mut clock_count = 0;
    for node in fdt.all_nodes() {
        if let NodeType::Clock(clock) = node {
            clock_count += 1;
            println!(
                "Clock node: {} #clock-cells={}",
                clock.path(),
                clock.clock_cells()
            );
        }
    }

    println!("Total clock nodes: {}", clock_count);
    // 飞腾 DTB 应该有时钟节点
    assert!(clock_count > 0, "phytium DTB should have clock nodes");
}

#[test]
fn test_clock_output_names() {
    let raw_data = fdt_phytium();
    let fdt = Fdt::from_bytes(&raw_data).unwrap();

    for node in fdt.all_nodes() {
        if let NodeType::Clock(clock) = node {
            let names = clock.clock_output_names();
            if !names.is_empty() {
                println!("Clock {} has output names: {:?}", clock.path(), names);

                // Test output_name method
                if let Some(first_name) = clock.output_name(0) {
                    assert_eq!(first_name, names[0]);
                    println!("  First output: {}", first_name);
                }
            }
        }
    }
}

#[test]
fn test_fixed_clock() {
    let raw_data = fdt_phytium();
    let fdt = Fdt::from_bytes(&raw_data).unwrap();

    for node in fdt.all_nodes() {
        if let NodeType::Clock(clock) = node {
            let clock_type = clock.clock_type();
            if let fdt_edit::ClockType::Fixed(fixed) = clock_type {
                println!("Fixed clock: {} freq={}Hz", clock.path(), fixed.frequency);

                // Fixed clock should have a frequency
                assert!(
                    fixed.frequency > 0 || fixed.accuracy.is_some(),
                    "Fixed clock should have frequency or accuracy"
                );

                if let Some(ref name) = fixed.name {
                    println!("  Name: {}", name);
                }

                if let Some(accuracy) = fixed.accuracy {
                    println!("  Accuracy: {} ppb", accuracy);
                }
            }
        }
    }
}

#[test]
fn test_clocks_property_parsing() {
    let raw_data = fdt_rpi_4b();
    let fdt = Fdt::from_bytes(&raw_data).unwrap();

    let mut found_clock_refs = false;
    for node in fdt.all_nodes() {
        if node.as_node().get_property("clocks").is_none() {
            continue;
        }

        let clocks = node.clocks();
        if clocks.is_empty() {
            continue;
        }

        found_clock_refs = true;
        println!("Node {} has {} clock references", node.path(), clocks.len());

        let clock_names: Vec<&str> = node
            .as_node()
            .get_property("clock-names")
            .map(|prop| prop.as_str_iter().collect())
            .unwrap_or_default();

        for (index, clock) in clocks.iter().enumerate() {
            println!(
                "  [{}] phandle={:?} cells={} specifier={:?} name={:?}",
                index, clock.phandle, clock.cells, clock.specifier, clock.name
            );

            assert_eq!(clock.specifier.len(), clock.cells as usize);

            if let Some(provider) = fdt.get_by_phandle(clock.phandle) {
                let provider_cells = provider
                    .as_node()
                    .get_property("#clock-cells")
                    .and_then(|prop| prop.get_u32())
                    .unwrap_or(1);
                assert_eq!(clock.cells, provider_cells);
            }

            if let Some(expected_name) = clock_names.get(index) {
                assert_eq!(clock.name.as_deref(), Some(*expected_name));
            }
        }
    }

    assert!(
        found_clock_refs,
        "should find nodes with parsable clocks property"
    );
}

#[test]
fn test_clock_ref_select() {
    let raw_data = fdt_rpi_4b();
    let fdt = Fdt::from_bytes(&raw_data).unwrap();

    let mut checked = false;
    for node in fdt.all_nodes() {
        for clock in node.clocks() {
            checked = true;
            assert_clock_select(&clock);
        }
    }

    assert!(
        checked,
        "should validate at least one parsed clock reference"
    );
}

fn assert_clock_select(clock: &ClockRef) {
    if clock.cells == 0 {
        assert_eq!(clock.select(), None);
    } else {
        assert_eq!(clock.select(), clock.specifier.first().copied());
    }
}
