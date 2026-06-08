use dtb_file::*;
use fdt_edit::{Fdt, NodeType, Phandle};

fn load_orangepi5plus() -> Fdt {
    let raw_data = fdt_orangepi_5plus();
    Fdt::from_bytes(&raw_data).unwrap()
}

#[test]
fn test_interrupt_controller_detection_and_properties() {
    let fdt = load_orangepi5plus();

    let root_irq = fdt.get_by_phandle(Phandle::from(0x01)).unwrap();
    let NodeType::InterruptController(intc) = root_irq else {
        panic!("phandle 0x01 should resolve to an interrupt controller");
    };

    assert!(intc.is_interrupt_controller());
    assert_eq!(intc.path(), "/interrupt-controller@fe600000");
    assert_eq!(intc.interrupt_cells(), Some(3));
    assert!(intc.compatibles().iter().any(|c| c.contains("gic")));
}

#[test]
fn test_interrupts_property_parsing() {
    let fdt = load_orangepi5plus();
    let gpu = fdt.get_by_path("/gpu@fb000000").unwrap();

    assert_eq!(gpu.interrupt_parent(), Some(Phandle::from(0x01)));

    let interrupts = gpu.interrupts();
    assert_eq!(interrupts.len(), 3);

    assert_eq!(interrupts[0].interrupt_parent, Phandle::from(0x01));
    assert_eq!(interrupts[0].cells, 3);
    assert_eq!(interrupts[0].specifier, vec![0x00, 0x5e, 0x04]);
    assert_eq!(interrupts[0].name.as_deref(), Some("GPU"));

    assert_eq!(interrupts[1].specifier, vec![0x00, 0x5d, 0x04]);
    assert_eq!(interrupts[1].name.as_deref(), Some("MMU"));

    assert_eq!(interrupts[2].specifier, vec![0x00, 0x5c, 0x04]);
    assert_eq!(interrupts[2].name.as_deref(), Some("JOB"));
}

#[test]
fn test_interrupts_inherit_parent() {
    let fdt = load_orangepi5plus();
    let uart = fdt.get_by_path("/serial@fd890000").unwrap();

    let interrupts = uart.interrupts();
    assert_eq!(uart.as_node().interrupt_parent(), None);
    assert_eq!(uart.interrupt_parent(), Some(Phandle::from(0x01)));
    assert_eq!(interrupts.len(), 1);
    assert_eq!(interrupts[0].interrupt_parent, Phandle::from(0x01));
    assert_eq!(interrupts[0].specifier, vec![0x00, 0x14b, 0x04]);
    assert_eq!(interrupts[0].name, None);
}
