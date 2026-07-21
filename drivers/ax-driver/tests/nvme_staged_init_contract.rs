use std::{fs, path::Path};

fn read_workspace_source(relative: &str) -> String {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("ax-driver must live under the workspace drivers directory");
    let path = workspace.join(relative);
    fs::read_to_string(&path).unwrap_or_else(|error| panic!("failed to read {path:?}: {error}"))
}

#[test]
fn nvme_discovery_registers_before_issuing_controller_commands() {
    let adapter = read_workspace_source("drivers/ax-driver/src/block/nvme.rs");
    let core = read_workspace_source("drivers/blk/nvme-driver/src/block/controller.rs");

    for forbidden in ["Nvme::new(", "NvmeBlockDriver::from_nvme("] {
        assert!(
            !adapter.contains(forbidden),
            "NVMe PCI discovery still enters the eager initializer through {forbidden}"
        );
    }
    let required = "NvmeBlockActivator::discover";
    assert!(
        adapter.contains(required),
        "NVMe discovery is missing the staged initialization boundary {required}"
    );
    for required in ["ControllerInitEndpoint::Pending", "InitialController"] {
        assert!(
            core.contains(required),
            "NVMe core is missing the staged initialization boundary {required}"
        );
    }
}

#[test]
fn nvme_initialization_and_normal_io_have_no_completion_polling_path() {
    let core = read_workspace_source("drivers/blk/nvme-driver/src/nvme.rs");
    let queue = read_workspace_source("drivers/blk/nvme-driver/src/queue.rs");
    let runtime = [
        "mod.rs",
        "adapter.rs",
        "core/mod.rs",
        "core/submission.rs",
        "core/completion_owner.rs",
        "dma.rs",
        "owned.rs",
        "prp.rs",
        "request/mod.rs",
    ]
    .into_iter()
    .map(|relative| {
        read_workspace_source(&format!(
            "drivers/blk/nvme-driver/src/block/queue_runtime/{relative}"
        ))
    })
    .collect::<Vec<_>>()
    .join("\n");

    for (source, name) in [
        (&core, "nvme.rs"),
        (&queue, "queue.rs"),
        (&runtime, "block/queue_runtime"),
    ] {
        for forbidden in ["command_sync", "spin_for_complete", "spin_loop"] {
            assert!(
                !source.contains(forbidden),
                "{name} retains an unbounded queue progress loop through {forbidden}"
            );
        }
    }
}

#[test]
fn nvme_capacity_and_queues_are_published_only_after_ready() {
    let block = read_workspace_source("drivers/blk/nvme-driver/src/block/controller.rs");

    for required in [
        "fn controller_init",
        "ControllerInitEndpoint::Pending",
        "fn namespace_if_ready",
        "fn create_queue",
    ] {
        assert!(
            block.contains(required),
            "NVMe block interface is missing ready-gated API {required}"
        );
    }
}

#[test]
fn nvme_recovery_reidentifies_controller_and_namespace_before_republication() {
    let lifecycle = read_workspace_source("drivers/blk/nvme-driver/src/lifecycle.rs");
    let controller = read_workspace_source("drivers/blk/nvme-driver/src/block/controller.rs");

    for required in [
        "AdminCommand::IdentifyController",
        "AdminCommand::IdentifyNamespaceList",
        "AdminCommand::IdentifyNamespace",
    ] {
        assert!(
            lifecycle.contains(required),
            "NVMe recovery can republish retained queues without {required}"
        );
    }
    assert!(
        controller.contains("complete_reinitialize_admin"),
        "NVMe recovery does not validate the newly identified geometry before republishing"
    );
}

#[test]
fn nvme_v13_recovery_consumes_the_linear_quiesce_proof() {
    let control = [
        read_workspace_source("drivers/blk/nvme-driver/src/block/v13/control.rs"),
        read_workspace_source("drivers/blk/nvme-driver/src/block/v13/recovery.rs"),
    ]
    .join("\n");

    assert!(
        !control.contains("NVMe v0.13 recovery requires"),
        "the v0.13 controller still rejects recovery instead of advancing its lifecycle"
    );
    for required in [
        "NvmeLifecycle",
        "DriverControlTrigger::BeginQuiesce",
        "DriverControlTrigger::BeginReinitialize",
        "ControlProgress::DmaQuiesced",
        "ControlProgress::Reinitialized",
    ] {
        assert!(
            control.contains(required),
            "NVMe v0.13 recovery is missing the linear lifecycle step {required}"
        );
    }
}

#[test]
fn nvme_queue_reset_precedes_controller_queue_recreation() {
    let domain = read_workspace_source("drivers/blk/nvme-driver/src/block/io_domain.rs");

    let reclaim = domain
        .find("fn reclaim_after_quiesce")
        .expect("the NVMe domain must reclaim under a DMA proof");
    let resume = domain[reclaim..]
        .find("fn resume_after_reinitialize")
        .map(|offset| reclaim + offset)
        .expect("the NVMe domain must consume a controller reinit permit");
    let reclaim_body = &domain[reclaim..resume];
    let resume_body = &domain[resume..];

    assert!(
        reclaim_body.contains("reset_after_quiesce"),
        "SQ/CQ cursors and CID epoch must reset while the matching DMA proof is borrowed"
    );
    assert!(
        !resume_body.contains("reset_after_reinitialize"),
        "queue memory cannot be reset after Create CQ/Create SQ has already rebuilt the controller"
    );
}

#[test]
fn nvme_initialization_failure_disables_or_quarantines_controller_dma() {
    let initialization = read_workspace_source("drivers/blk/nvme-driver/src/initialization.rs");

    for required in [
        "InitializationState::Aborting",
        "fn begin_abort",
        "hardware.begin_controller_disable()",
        "fn poll_aborting",
        "!hardware.controller_ready() || now_ns >= deadline_ns",
        "fn publish_ready",
    ] {
        assert!(
            initialization.contains(required),
            "NVMe init failure can escape with live controller DMA without {required}"
        );
    }

    let publish = initialization
        .find("hardware.publish_ready()")
        .expect("namespace publication must remain in the init transaction");
    let ready = initialization[publish..]
        .find("self.state = InitializationState::Ready")
        .map(|offset| publish + offset)
        .expect("ready publication must have a terminal state transition");
    assert!(
        publish < ready,
        "namespace publication failure must enter abort before Ready is visible"
    );
}

#[test]
fn nvme_does_not_fallback_to_intx_after_a_failed_msix_transaction() {
    let adapter = read_workspace_source("drivers/ax-driver/src/block/nvme.rs");

    assert!(adapter.contains("Err(OnProbeError::Unsupported(reason))"));
    assert!(
        adapter.contains("Err(err) => return Err(err)"),
        "an MSI-X programming or rollback failure must stop probe instead of activating INTx on \
         an endpoint with unproven interrupt state"
    );
}

#[test]
fn nvme_msix_lease_retains_the_pci_endpoint_until_shutdown() {
    let adapter = read_workspace_source("drivers/ax-driver/src/block/nvme.rs");
    let lease = read_workspace_source("drivers/ax-driver/src/pci/msi/lease.rs");

    assert!(
        lease.contains("endpoint: Option<Endpoint>"),
        "the MSI-X lease must retain exclusive ownership of the PCI endpoint"
    );
    for required in [
        "let endpoint = probe.take_endpoint()",
        "preflight.activate(endpoint, vector_count)",
        "register_deferred_irq_block_activator",
    ] {
        assert!(
            adapter.contains(required),
            "NVMe MSI-X publication can outlive its PCI endpoint ownership without {required}"
        );
    }
}

#[test]
fn nvme_msix_setup_and_probe_failure_keep_one_fail_closed_lease() {
    let adapter = read_workspace_source("drivers/ax-driver/src/block/nvme.rs");
    let activation = read_workspace_source("drivers/ax-driver/src/pci/msi/activation.rs");
    let lease = read_workspace_source("drivers/ax-driver/src/pci/msi/lease.rs");
    let transaction = read_workspace_source("drivers/ax-driver/src/pci/msi/transaction.rs");

    let preflight = adapter
        .find("PciIrqLease::preflight")
        .expect("NVMe must validate the policy-independent MSI-X ceiling");
    let discover = adapter
        .find("discover_msix_activator(bar.clone(), max_queue_pairs)")
        .expect("NVMe staged discovery must advertise the maximum topology");
    let take = adapter
        .find("let endpoint = probe.take_endpoint()")
        .expect("the deferred platform owner must take the PCI endpoint once");
    let register = adapter
        .find("register_deferred_irq_block_activator")
        .expect("runtime plan selection must precede physical MSI-X allocation");
    assert!(
        preflight < discover && discover < take && take < register,
        "discovery must retain only the MSI-X ceiling and defer allocation until runtime selection"
    );
    assert!(adapter.contains("match preflight.activate(endpoint, vector_count)"));

    for required in [
        "rollback_msix_setup_steps",
        "MsixSetupRollbackStep::FunctionMask",
        "MsixSetupRollbackStep::TableEntry",
        "MsixSetupRollbackStep::ProviderVector",
        "MsixSetupRollbackStep::DisableCapability",
    ] {
        assert!(
            transaction.contains(required) || lease.contains(required),
            "MSI-X setup rollback is missing the fail-closed step {required}"
        );
    }
    assert!(
        lease.contains("set_msix_enabled(false)"),
        "MSI-X lease release must disable the endpoint capability before freeing vectors"
    );
    assert!(
        activation.contains("PciMsiQuarantineReason::SetupContainment"),
        "an incomplete setup rollback must retain vector and table ownership"
    );

    let lease_drop = lease
        .find("impl Drop for PciIrqLease")
        .map(|offset| &lease[offset..])
        .expect("MSI-X lease must own an explicit shutdown transaction");
    let capability_disable = lease_drop
        .find("set_msix_enabled(false)")
        .expect("lease shutdown must disable the MSI-X capability");
    let quarantine = lease_drop
        .find("retain_quarantined_resources")
        .expect("lease shutdown must quarantine incomplete cleanup");
    assert!(
        capability_disable < quarantine,
        "lease shutdown must attempt endpoint-wide containment even when vector disable fails"
    );
}

#[test]
fn nvme_queue_runtime_is_a_small_domain_directory() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("ax-driver must live under the workspace drivers directory");
    let legacy_entry = workspace.join("drivers/blk/nvme-driver/src/block/queue_runtime.rs");
    let domain = workspace.join("drivers/blk/nvme-driver/src/block/queue_runtime");
    let entry = domain.join("mod.rs");

    assert!(
        !legacy_entry.exists(),
        "queue_runtime with children must use queue_runtime/mod.rs"
    );
    let source = fs::read_to_string(&entry)
        .unwrap_or_else(|error| panic!("NVMe queue runtime entry {entry:?}: {error}"));
    assert!(
        source.lines().count() <= 120,
        "queue_runtime/mod.rs must remain a directory page"
    );
    for module in ["adapter", "core", "dma", "owned", "prp", "request"] {
        assert!(
            source.contains(&format!("mod {module};")),
            "queue runtime is missing the {module} responsibility module"
        );
    }
    for relative in [
        "adapter.rs",
        "core/mod.rs",
        "core/submission.rs",
        "core/completion_owner.rs",
        "dma.rs",
        "owned.rs",
        "prp.rs",
        "request/mod.rs",
        "request/tests.rs",
    ] {
        let leaf = domain.join(relative);
        let leaf_source = fs::read_to_string(&leaf)
            .unwrap_or_else(|error| panic!("NVMe queue runtime leaf {leaf:?}: {error}"));
        assert!(
            leaf_source.lines().count() <= 400,
            "{relative} still mixes responsibilities at {} lines",
            leaf_source.lines().count()
        );
    }
}
