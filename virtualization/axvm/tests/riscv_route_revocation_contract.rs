use std::{fs, path::PathBuf};

fn workspace_file(relative: &str) -> String {
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|path| path.parent())
        .expect("axvm must live under the workspace virtualization directory")
        .to_path_buf();
    fs::read_to_string(workspace.join(relative)).expect("contract source must be readable")
}

#[test]
fn riscv_route_publication_is_generation_scoped_and_retryable() {
    let axvm_irq = workspace_file("virtualization/axvm/src/arch/riscv64/irq.rs");
    let axvm_arch = workspace_file("virtualization/axvm/src/arch/riscv64/mod.rs");

    assert!(!axvm_irq.contains("static PLATFORM_VPLIC_ROUTE: Once"));
    for contract in [
        "PlatformVplicRouteSlot",
        "ROUTE_REVOKING",
        "publishers_drained",
        "platform_released",
        "revoke_forwarded_route_batch",
        "finish_platform_revocation",
    ] {
        assert!(
            axvm_irq.contains(contract),
            "missing AxVM contract {contract}"
        );
    }
    assert!(axvm_arch.contains("fn revoke_guest_irq_routes"));
}

#[test]
fn plic_release_drains_claims_and_clears_the_generation_lease() {
    let platform = workspace_file("platforms/axplat-dyn/src/irq.rs");
    let plic = workspace_file("platforms/somehal/src/arch/riscv64/plic.rs");
    let vplic = workspace_file("virtualization/riscv_vplic/src/devops_impl.rs");

    assert!(!platform.contains("[spin::Once<RiscvVirtualIrqEndpoint>"));
    for contract in [
        "Riscv64HvIrqIf for IrqIfImpl",
        "begin_virtual_irq_route_revocation",
        "poll_virtual_irq_route_revocation",
        "release_riscv_plic_irq_endpoints",
    ] {
        assert!(
            platform.contains(contract),
            "missing platform contract {contract}"
        );
    }
    for contract in [
        "PLIC_CLAIM_READERS",
        "ACTIVE_PLIC_CLAIMS",
        "lease_generation_by_source",
        "self.leased_by_source[source_index] = false",
        "self.disable_source_contexts(source)",
    ] {
        assert!(plic.contains(contract), "missing PLIC contract {contract}");
    }
    assert!(vplic.contains("set_forwarded_pending_batch_for_generation"));
    assert!(vplic.contains("revoke_forwarded_route_batch"));
}
