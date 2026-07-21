use std::{fs, path::Path};

#[test]
fn pci_msi_failure_retains_resources_in_a_bounded_named_registry() {
    let transaction = read_source("drivers/ax-driver/src/pci/msi/transaction.rs");
    let quarantine = read_source("drivers/ax-driver/src/pci/msi/quarantine.rs");
    let activation = read_source("drivers/ax-driver/src/pci/msi/activation.rs");
    let lease = read_source("drivers/ax-driver/src/pci/msi/lease.rs");

    assert!(
        !transaction.contains("mem::forget")
            && !activation.contains("mem::forget")
            && !lease.contains("mem::forget"),
        "MSI rollback must retain typed owners instead of anonymously leaking them"
    );
    for required in [
        "PciMsiQuarantineRegistry",
        "PciMsiQuarantineReservation",
        "PCI_MSI_QUARANTINE_CAPACITY",
        "MsiQuarantinedResources",
    ] {
        assert!(
            quarantine.contains(required),
            "MSI teardown is missing the named ownership primitive {required}"
        );
    }
    assert!(
        activation
            .find("PciMsiQuarantineReservation::reserve(self.info.address)")
            .unwrap()
            < activation.find("provider.allocate").unwrap(),
        "quarantine capacity must be reserved before the provider transfers vector ownership"
    );
}

#[test]
fn failed_provider_release_returns_the_allocation_owner() {
    let interface = read_source("drivers/interface/rdif-msi/src/lib.rs");

    assert!(interface.contains("pub struct MsiFreeFailure"));
    assert!(interface.contains("allocation: MsiAllocation"));
    assert!(
        interface.contains("Result<(), MsiFreeFailure>"),
        "provider release failure must return the linear allocation owner"
    );
}

#[test]
fn msix_activation_is_deferred_until_the_runtime_selects_a_plan() {
    let lease = read_source("drivers/ax-driver/src/pci/msi/lease.rs");
    let activation = read_source("drivers/ax-driver/src/pci/msi/activation.rs");
    let nvme = read_source("drivers/ax-driver/src/block/nvme.rs");

    for required in [
        "pub(crate) struct PciMsixPreflight",
        "pub(crate) enum PciMsixActivationFailure",
        "pub(crate) fn preflight(",
        "endpoint: &Endpoint",
    ] {
        assert!(
            lease.contains(required),
            "MSI-X ownership transaction is missing {required}"
        );
    }
    for required in ["pub(crate) fn activate(", "mut endpoint: Endpoint"] {
        assert!(
            activation.contains(required),
            "MSI-X ownership activation is missing {required}"
        );
    }
    assert!(
        !lease.contains("retain_endpoint") && !activation.contains("retain_endpoint"),
        "a live MSI-X lease must own the endpoint from activation onward"
    );

    let preflight = nvme
        .find("PciIrqLease::preflight")
        .expect("NVMe must preflight MSI-X while ProbePci still owns the endpoint");
    let take = nvme
        .find("probe.take_endpoint()")
        .expect("NVMe must transfer the endpoint before MSI-X activation");
    let discover = nvme
        .find("discover_msix_activator(bar.clone(), max_queue_pairs)")
        .expect("NVMe must advertise the hardware ceiling before taking the endpoint");
    let register = nvme
        .find("register_deferred_irq_block_activator")
        .expect("NVMe must transfer the endpoint into a deferred IRQ owner");
    assert!(preflight < discover && discover < take && take < register);
    assert!(
        nvme.contains("match preflight.activate(endpoint, vector_count)")
            && nvme.contains("PlatformIrqActivationFailure::returned"),
        "a fully rolled-back plan-selected activation must return the complete deferred owner"
    );
    assert!(
        nvme.contains("OnProbeError::claimed"),
        "an incompletely contained activation must terminate probing as claimed"
    );
}

#[test]
fn rdrive_claimed_probe_state_never_reuses_a_consumed_endpoint() {
    let error = read_source("drivers/rdrive/src/probe/mod.rs");
    let pci = read_source("drivers/rdrive/src/probe/pci/mod.rs");

    assert!(error.contains("Claimed"));
    assert!(error.contains("pub fn claimed"));
    assert!(pci.contains("pub fn restore_endpoint"));
    assert!(pci.contains("endpoint.is_available()"));
    assert!(pci.contains("FailedPciProbe::Claimed"));
}

fn read_source(path: &str) -> String {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("ax-driver must live below the workspace drivers directory");
    fs::read_to_string(workspace.join(path))
        .unwrap_or_else(|error| panic!("failed to read {path}: {error}"))
}
