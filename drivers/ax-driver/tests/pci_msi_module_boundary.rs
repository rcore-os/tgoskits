use std::{fs, path::PathBuf};

#[test]
fn pci_msi_domain_is_split_by_lease_transaction_and_routing_responsibility() {
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/pci");
    let legacy = source.join("msi.rs");
    let domain = source.join("msi");

    assert!(
        !legacy.exists(),
        "MSI with child responsibilities must use msi/mod.rs"
    );
    for (leaf, limit) in [
        ("mod.rs", 500),
        ("activation.rs", 500),
        ("lease.rs", 500),
        ("routing.rs", 300),
        ("transaction.rs", 300),
        ("tests.rs", 500),
    ] {
        let path = domain.join(leaf);
        let contents = fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("failed to read PCI MSI module {path:?}: {error}"));
        assert!(
            contents.lines().count() <= limit,
            "{leaf} exceeds its {limit}-line responsibility budget"
        );
    }

    let entry = fs::read_to_string(domain.join("mod.rs")).expect("MSI domain entry must exist");
    for declaration in [
        "mod activation;",
        "mod lease;",
        "mod routing;",
        "mod transaction;",
    ] {
        assert!(entry.contains(declaration), "missing {declaration}");
    }

    let lease = fs::read_to_string(domain.join("lease.rs")).expect("MSI lease module must exist");
    let routing =
        fs::read_to_string(domain.join("routing.rs")).expect("MSI routing module must exist");
    assert!(
        routing.contains("pub struct PciMsiTarget"),
        "routing results must be owned by the routing module"
    );
    assert!(
        !lease.contains("pub struct PciMsiTarget"),
        "the lease module must consume, not define, routing results"
    );
}
