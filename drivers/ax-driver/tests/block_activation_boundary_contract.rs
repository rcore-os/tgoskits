use std::{fs, path::Path};

fn source(relative: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(relative);
    fs::read_to_string(&path).unwrap_or_else(|error| panic!("failed to read {path:?}: {error}"))
}

#[test]
fn registration_rejects_unresolved_declared_irq_sources() {
    let binding = source("src/block/binding.rs");
    let activation_binding = source("src/block/activation_binding.rs");
    let staged = source("src/block/staged.rs");
    let virtio = source("src/virtio/block/discovery.rs");

    assert!(binding.contains("validate_controller_irq_bindings(&mut bundle, &binding)"));
    assert!(binding.contains("BlockRegistrationError::MissingIrqBinding"));
    assert!(binding.contains("BlockRegistrationError::InterruptControllerWithoutIrqSource"));
    assert!(binding.contains("LifecycleKind::Interrupt"));
    assert!(binding.contains("initializer.irq_sources()"));
    assert!(binding.contains("bundle.irq_sources()"));
    assert!(activation_binding.contains("validate_fact_only_activator_registration"));
    assert!(activation_binding.contains("validate_activator_irq_bindings"));
    assert!(activation_binding.contains("validate_irq_source_bindings"));
    assert!(activation_binding.contains("BlockRegistrationError::MissingIrqBinding"));
    assert!(virtio.contains("register_block_activator_with_info"));

    for required in ["BlockIrqSource", "take_irq_source"] {
        assert!(
            staged.contains(required),
            "staged controller wrapper does not transfer split IRQ capability `{required}`"
        );
    }
    for forbidden in [
        "BIrqHandler",
        "IrqHandler",
        "take_irq_handler",
        "service_deferred_irq",
        "InitIrqProgress",
    ] {
        assert!(
            !binding.contains(forbidden) && !staged.contains(forbidden),
            "generic block binding retained obsolete IRQ ABI `{forbidden}`"
        );
    }
}

#[test]
fn pci_intx_block_controllers_retain_a_move_only_endpoint_lease() {
    let pci = source("src/pci/intx.rs");
    let ahci = source("src/block/ahci.rs");
    let nvme = source("src/block/nvme.rs");
    let virtio = source("src/virtio/block/discovery.rs");

    for required in [
        "pub struct PciIntxIrqLease",
        "impl IrqBindingLease for PciIntxIrqLease",
        "mask_intx_command",
        "unmask_intx_command",
    ] {
        assert!(
            pci.contains(required),
            "missing PCI INTx lease contract {required}"
        );
    }

    assert!(ahci.contains("register_irq_bound_block_activator"));
    assert!(ahci.contains("PciIntxIrqLease"));
    assert!(nvme.contains("register_irq_bound_block_activator"));
    assert!(nvme.contains("PciIntxIrqLease"));
    assert!(virtio.contains("take_virtio_block_transport"));
    assert!(virtio.contains("register_irq_bound_block_activator"));

    for (name, source) in [("AHCI", ahci), ("NVMe", nvme)] {
        assert!(
            !source.contains("remove(CommandRegister::INTERRUPT_DISABLE)"),
            "{name} discovery unmasks PCI INTx before the runtime owns the IRQ action"
        );
    }
    assert!(
        !source("src/block/nvme.rs").contains("register_irq_bound_block(driver"),
        "NVMe production probe must not retain the legacy block registration path"
    );
}
