#[test]
fn guest_plic_registers_never_alias_the_physical_controller() {
    let model = include_str!("../src/vplic.rs");
    let mmio = include_str!("../src/devops_impl.rs");

    for forbidden in [
        "host_plic_addr",
        "perform_mmio_read",
        "perform_mmio_write",
        "RiscvVplicHostIf",
    ] {
        assert!(
            !model.contains(forbidden) && !mmio.contains(forbidden),
            "guest PLIC state must be software-owned and must not use {forbidden}"
        );
    }

    for state in ["priorities", "context_enables", "context_thresholds"] {
        assert!(
            model.contains(state),
            "the software vPLIC must own {state} independently of the host controller"
        );
    }
}

#[test]
fn forwarded_completion_is_preallocated_software_state() {
    let model = include_str!("../src/vplic.rs");
    let mmio = include_str!("../src/devops_impl.rs");

    assert!(model.contains("forwarded_irqs"));
    assert!(model.contains("completed_forwarded_irqs"));
    assert!(mmio.contains("set_forwarded_pending"));
    assert!(mmio.contains("take_completed_forwarded_irq"));
    assert!(
        !mmio.contains("Vec::") && !mmio.contains("Box::new"),
        "forward and completion paths must use state preallocated by VPlicGlobal::new"
    );
}
