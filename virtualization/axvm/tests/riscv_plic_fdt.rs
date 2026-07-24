extern crate alloc;

use alloc::{format, vec, vec::Vec};

use fdt_edit::{Fdt, Node, Property};

#[path = "../src/arch/riscv64/fdt/plic.rs"]
mod plic;

fn prop_u32(name: &str, value: u32) -> Property {
    let mut prop = Property::new(name, vec![]);
    prop.set_u32_ls(&[value]);
    prop
}

fn prop_u32_list(name: &str, values: &[u32]) -> Property {
    let mut prop = Property::new(name, vec![]);
    prop.set_u32_ls(values);
    prop
}

fn prop_string(name: &str, value: &str) -> Property {
    let mut prop = Property::new(name, vec![]);
    prop.set_string(value);
    prop
}

fn four_cpu_plic_fdt() -> Fdt {
    let mut fdt = Fdt::new();
    let cpus = fdt.add_node(fdt.root_id(), Node::new("cpus"));

    for cpu_id in 0..4 {
        let cpu = fdt.add_node(cpus, Node::new(&format!("cpu@{cpu_id}")));
        fdt.node_mut(cpu)
            .unwrap()
            .set_property(prop_u32("phandle", cpu_id + 1));
        let intc = fdt.add_node(cpu, Node::new("interrupt-controller"));
        fdt.node_mut(intc)
            .unwrap()
            .set_property(prop_u32("phandle", 0x10 + cpu_id));
        fdt.node_mut(intc)
            .unwrap()
            .set_property(prop_u32("#interrupt-cells", 1));
    }

    let soc = fdt.add_node(fdt.root_id(), Node::new("soc"));
    let plic = fdt.add_node(soc, Node::new("plic@c000000"));
    fdt.node_mut(plic)
        .unwrap()
        .set_property(prop_string("compatible", "riscv,plic0"));
    fdt.node_mut(plic).unwrap().set_property(prop_u32_list(
        "interrupts-extended",
        &[
            0x10, 11, 0x10, 9, 0x11, 11, 0x11, 9, 0x12, 11, 0x12, 9, 0x13, 11, 0x13, 9,
        ],
    ));

    Fdt::from_bytes(fdt.encode().as_ref()).unwrap()
}

fn property_cells(fdt: &Fdt, node_path: &str, property: &str) -> Vec<u32> {
    fdt.get_by_path(node_path)
        .unwrap()
        .as_node()
        .get_property(property)
        .unwrap()
        .get_u32_iter()
        .collect()
}

fn set_plic_contexts(fdt: &mut Fdt, contexts: &[u32]) {
    let plic = fdt.get_by_path_id("/soc/plic@c000000").unwrap();
    fdt.node_mut(plic)
        .unwrap()
        .set_property(prop_u32_list("interrupts-extended", contexts));
}

#[test]
fn riscv_plic_keeps_only_retained_cpu_contexts() {
    let host = four_cpu_plic_fdt();
    let mut guest = host.clone();
    for cpu_id in [0, 1, 3] {
        guest.remove_by_path(&format!("/cpus/cpu@{cpu_id}"));
    }

    plic::normalize_interrupts_extended(&host, &mut guest).unwrap();

    assert_eq!(
        property_cells(&guest, "/soc/plic@c000000", "interrupts-extended"),
        [0x12, 11, 0x12, 9]
    );
}

#[test]
fn riscv_plic_rejects_missing_interrupt_provider() {
    let mut host = four_cpu_plic_fdt();
    set_plic_contexts(&mut host, &[0x99, 9]);
    let mut guest = host.clone();

    assert!(matches!(
        plic::normalize_interrupts_extended(&host, &mut guest),
        Err(plic::PlicFdtError::MissingProvider { phandle: 0x99 })
    ));
}

#[test]
fn riscv_plic_rejects_missing_interrupt_cell_count() {
    let mut host = four_cpu_plic_fdt();
    let provider = host
        .get_by_path_id("/cpus/cpu@0/interrupt-controller")
        .unwrap();
    host.node_mut(provider)
        .unwrap()
        .remove_property("#interrupt-cells");
    set_plic_contexts(&mut host, &[0x10, 9]);
    let mut guest = host.clone();

    assert!(matches!(
        plic::normalize_interrupts_extended(&host, &mut guest),
        Err(plic::PlicFdtError::MissingInterruptCells { phandle: 0x10 })
    ));
}

#[test]
fn riscv_plic_rejects_zero_interrupt_cell_count() {
    let mut host = four_cpu_plic_fdt();
    let provider = host
        .get_by_path_id("/cpus/cpu@0/interrupt-controller")
        .unwrap();
    host.node_mut(provider)
        .unwrap()
        .set_property(prop_u32("#interrupt-cells", 0));
    set_plic_contexts(&mut host, &[0x10, 9]);
    let mut guest = host.clone();

    assert!(matches!(
        plic::normalize_interrupts_extended(&host, &mut guest),
        Err(plic::PlicFdtError::ZeroInterruptCells { phandle: 0x10 })
    ));
}

#[test]
fn riscv_plic_rejects_truncated_context_tuple() {
    let mut host = four_cpu_plic_fdt();
    set_plic_contexts(&mut host, &[0x10]);
    let mut guest = host.clone();

    assert!(matches!(
        plic::normalize_interrupts_extended(&host, &mut guest),
        Err(plic::PlicFdtError::TruncatedTuple {
            phandle: 0x10,
            expected_cells: 1,
            remaining_cells: 0,
        })
    ));
}
