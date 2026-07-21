use std::{fs, path::PathBuf};

fn source(relative: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative);
    fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("virtio block source {path:?}: {error}"))
}

/// Production v0.13 sources. The cfg(test) v0.12 fixtures in `queue/legacy.rs`,
/// `controller.rs`, and `lifecycle.rs` are intentionally excluded.
fn virtio_block_source() -> String {
    [
        "src/virtio/block/mod.rs",
        "src/virtio/block/discovery.rs",
        "src/virtio/block/device.rs",
        "src/virtio/block/initialization.rs",
        "src/virtio/block/irq.rs",
        "src/virtio/block/notify.rs",
        "src/virtio/block/queue.rs",
        "src/virtio/block/queue/owned.rs",
        "src/virtio/block/v13.rs",
        "src/virtio/block/v13/activation.rs",
        "src/virtio/block/v13/evidence.rs",
        "src/virtio/block/v13/io.rs",
    ]
    .into_iter()
    .map(source)
    .collect::<Vec<_>>()
    .join("\n")
}

fn virtio_common_source() -> String {
    source("src/virtio/mod.rs")
}

#[test]
fn virtio_block_uses_v013_interrupt_owned_request_contract() {
    let source = virtio_block_source();

    for required in [
        "QueueKind::Interrupt",
        "QueueExecution::Tagged",
        "HardwareQueueDepth::fixed(NonZeroU16::MIN)",
        "Result<AcceptedRequest, UnacceptedRequest>",
        "fn service_evidence(",
        "EvidenceServiceResult",
        "IrqEvidenceId",
        "CompletedRequest",
        "fn shutdown(&mut self) -> Result<(), BlkError>",
    ] {
        assert!(
            source.contains(required),
            "missing rdif-block 0.13 contract: {required}"
        );
    }

    for forbidden in [
        "poll_request",
        "poll_completions",
        "QueueEventBatch",
        "ServiceRerunReason",
        "SubmitOutcome",
        "RequestPoller",
        "next_request_id",
        "queue_work_on",
        "workqueue",
        "Waker",
    ] {
        assert!(
            !source.contains(forbidden),
            "legacy polling or OS-policy contract remains: {forbidden}"
        );
    }
}

#[test]
fn irq_status_is_owned_by_a_split_evidence_and_notify_port() {
    let source = virtio_block_source();

    for required in [
        "VirtioInterruptPort",
        "VirtioQueueNotifyPort",
        "BlockEvidenceSource::new",
        "impl IrqEndpoint",
        "fn capture(&mut self)",
        "fn capture_raw_status(&mut self)",
        "IrqEvidenceId",
        "VirtioBlockEvidenceLedger",
        "IrqCapture::Unhandled",
        "Arc<mmio_api::Mmio>",
    ] {
        assert!(
            source.contains(required),
            "missing split VirtIO IRQ evidence contract: {required}"
        );
    }

    for forbidden in [
        "take_irq_handler",
        "IrqOutcome",
        "DeferredIrqProgress",
        "continue_deferred",
        "InitIrqProgress",
        "service_deferred_irq",
        "irq_ack_pending",
        "transport.ack_interrupt",
        "MmioRaw",
    ] {
        assert!(
            !source.contains(forbidden),
            "VirtIO IRQ still borrows the controller or exposes deferred acknowledgement: \
             {forbidden}"
        );
    }
}

#[test]
fn queue_doorbell_is_bound_before_requests_can_become_visible() {
    let source = virtio_block_source();

    for required in [
        "struct BoundVirtioQueueNotifyPort",
        "Result<BoundVirtioQueueNotifyPort, (BlkError, Self)>",
        "notify: BoundVirtioQueueNotifyPort",
        "self.notify.notify();",
    ] {
        assert!(
            source.contains(required),
            "VirtIO queue publication lacks the bound doorbell proof: {required}"
        );
    }
    for forbidden in ["notification_fault", "!self.notify.notify("] {
        assert!(
            !source.contains(forbidden),
            "accepted VirtIO requests still depend on a fallible late doorbell: {forbidden}"
        );
    }
}

#[test]
fn discovery_defers_all_virtio_initialization_until_activation() {
    let production = virtio_block_source();
    let discovery = source("src/virtio/block/discovery.rs");

    for required in [
        "impl<T: VirtIoTransport> ControllerActivator",
        "VirtioBlockActivator::discovered",
        "ControllerControlPart::new_combined_shared",
        "VirtioBlockInitPhase",
        "InitSchedule::immediate()",
        "register_block_activator_with_info",
    ] {
        assert!(
            production.contains(required),
            "missing two-phase VirtIO activation contract: {required}"
        );
    }

    for forbidden in ["VirtIOBlk::new", ".poll_init(", "read_consistent("] {
        assert!(
            !discovery.contains(forbidden),
            "discovery still enters hardware initialization: {forbidden}"
        );
    }
}

#[test]
fn pci_discovery_captures_memory_bars_before_transport_takeover() {
    let source = source("src/virtio/block/discovery.rs");
    let probe = source
        .find("fn probe_pci")
        .expect("VirtIO block must expose PCI discovery");
    let probe_source = &source[probe..];
    let binding = probe_source
        .find("let info = binding_info_from_pci_endpoint(")
        .expect("VirtIO block PCI discovery must preserve endpoint BAR identity");
    let transport = probe_source
        .find("crate::pci::take_virtio_block_transport")
        .expect("VirtIO block PCI discovery must create a transport");

    assert!(
        binding < transport,
        "memory BARs must be copied before transport construction mutates the endpoint"
    );
    assert!(
        !probe_source.contains("binding_info_from_pci(probe.info()"),
        "IRQ-only PCI binding loses the MMIO identity required for exact passthrough selection"
    );
}

#[test]
fn mmio_and_pci_preserve_isr_and_notify_capabilities_before_transport_erasure() {
    let block = virtio_block_source();
    let discovery = source("src/virtio/block/discovery.rs");
    let common = virtio_common_source();

    let pci_isr = discovery
        .find("let interrupt_port = pci_interrupt_port(probe.endpoint())")
        .expect("PCI probe must extract the dedicated ISR capability");
    let pci_notify = discovery
        .find("let notify_port = pci_notify_port(probe.endpoint())")
        .expect("PCI probe must extract the dedicated notify capability");
    let pci_transport = discovery
        .find("crate::pci::take_virtio_block_transport")
        .expect("PCI probe must still construct the queue/config transport");
    assert!(pci_isr < pci_transport && pci_notify < pci_transport);

    for required in [
        "VirtioInterruptPort::from_pci_isr",
        "VirtioInterruptPort::from_mmio",
        "VirtioQueueNotifyPort::from_pci",
        "VirtioQueueNotifyPort::from_mmio",
        "register_transport_with_interrupt_port",
        "block::register_mmio_transport",
        "if bar >= 6",
        "capability_length < PCI_VIRTIO_ISR_CAP_MIN_LENGTH",
    ] {
        assert!(
            block.contains(required) || common.contains(required),
            "block registration erased an interrupt/notify capability: {required}"
        );
    }
    assert!(
        common.find("block::register_mmio_transport").unwrap()
            < common
                .find("register_static_transport(plat_dev, ty, transport)")
                .unwrap(),
        "static MMIO block registration must split IRQ status before generic dispatch"
    );
}

#[test]
fn queue_and_capacity_are_moved_only_after_ready() {
    let source = virtio_block_source();

    for required in [
        "VirtioOwnedQueue::take_ready",
        "inner.init_phase != crate::virtio::block::initialization::VirtioBlockInitPhase::Ready",
        "inner.queue.take()",
        "inner.descriptor_storage.take()",
        "let info = queue.info()",
    ] {
        assert!(
            source.contains(required),
            "ready publication does not linearly move queue state: {required}"
        );
    }
}

#[test]
fn recovery_requires_acknowledged_dma_quiescence_and_fails_closed_without_rebuild_parts() {
    let source = virtio_block_source();

    for required in [
        "rdif_block::DmaQuiesced::new",
        "set_status(virtio_drivers::transport::DeviceStatus::empty())",
        "VirtioV13Lifecycle::Quiesced",
        "fn reclaim_after_quiesce(",
        "fn resume_after_reinitialize",
        "fn begin_rebuild(",
        "fn install_rebuilt_queue(",
    ] {
        assert!(
            source.contains(required),
            "VirtIO lifecycle can reclaim or resume without proof: {required}"
        );
    }
}

#[test]
fn initialization_failure_quiesces_or_quarantines_dma_before_terminal_failure() {
    let source = virtio_block_source();

    for required in [
        "VirtioBlockInitPhase::FailureReset",
        "fn begin_failure_reset",
        "self.set_interrupts(false)",
        "self.transport.set_status(DeviceStatus::empty())",
        "drop(self.queue.take())",
        "fn quarantine_unproven_dma",
        "struct VirtioDmaQuarantine",
        "VirtioDmaQuarantineReason",
        "ManuallyDrop<VirtQueue",
        "dma_quarantine",
    ] {
        assert!(
            source.contains(required),
            "VirtIO init failure can release live DMA without {required}"
        );
    }

    for forbidden in ["mem::forget", "Box::leak"] {
        assert!(
            !source.contains(forbidden),
            "VirtIO DMA quarantine must retain named ownership instead of anonymously leaking: \
             {forbidden}"
        );
    }
}

#[test]
fn irq_masking_does_not_negotiate_event_idx_without_a_suppression_api() {
    let source = virtio_block_source();

    assert!(
        !source.contains("VIRTIO_F_RING_EVENT_IDX"),
        "VirtQueue::set_dev_notify(false) is a no-op with EVENT_IDX, so reset cannot prove \
         device-side notification masking"
    );
}

#[test]
fn virtio_block_entry_is_a_small_domain_directory() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let legacy_entry = manifest.join("src/virtio/block.rs");
    let domain_entry = manifest.join("src/virtio/block/mod.rs");

    assert!(
        !legacy_entry.exists(),
        "a parent module with children must use block/mod.rs, not block.rs"
    );
    let source = fs::read_to_string(&domain_entry)
        .unwrap_or_else(|error| panic!("virtio block domain entry {domain_entry:?}: {error}"));
    assert!(
        source.lines().count() <= 200,
        "block/mod.rs must remain a directory page, got {} lines",
        source.lines().count()
    );
    for module in [
        "controller",
        "discovery",
        "initialization",
        "irq",
        "lifecycle",
        "notify",
        "queue",
        "v13",
    ] {
        assert!(
            source.contains(&format!("mod {module};")),
            "block/mod.rs is missing the {module} responsibility module"
        );
    }
    for implementation in [
        "struct VirtIoBlkDevice",
        "impl<T: VirtIoTransport",
        "fn probe_pci",
        "fn submit_owned",
        "fn capture_raw_status",
    ] {
        assert!(
            !source.contains(implementation),
            "block/mod.rs mixes implementation responsibility {implementation}"
        );
    }
}
