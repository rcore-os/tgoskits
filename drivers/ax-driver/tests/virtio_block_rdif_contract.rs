use std::{fs, path::PathBuf};

fn virtio_block_source() -> String {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let domain = manifest.join("src/virtio/block");
    [
        "discovery.rs",
        "controller.rs",
        "device.rs",
        "initialization.rs",
        "irq.rs",
        "lifecycle.rs",
        "queue.rs",
    ]
    .into_iter()
    .map(|module| {
        let path = domain.join(module);
        fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("virtio block source {path:?}: {error}"))
    })
    .collect::<Vec<_>>()
    .join("\n")
}

fn virtio_common_source() -> String {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    fs::read_to_string(manifest.join("src/virtio/mod.rs"))
        .expect("virtio common source must be readable")
}

#[test]
fn virtio_block_uses_interrupt_owned_request_contract() {
    let source = virtio_block_source();

    for required in [
        "QueueKind::Interrupt",
        "QueueExecution::Tagged",
        "submit_owned",
        "service_events",
        "QueueEventBatch",
        "fn shutdown(&mut self) -> Result<(), BlkError>",
        "CompletedRequest",
    ] {
        assert!(
            source.contains(required),
            "missing rdif-block 0.12 contract: {required}"
        );
    }

    for forbidden in [
        "poll_request",
        "RequestStatus",
        "next_request_id",
        "shutdown(&mut self, sink",
        "shutdown(&mut self, _sink",
    ] {
        assert!(
            !source.contains(forbidden),
            "legacy polling/driver-ID contract remains: {forbidden}"
        );
    }
}

#[test]
fn irq_status_is_owned_by_a_split_capture_and_control_port() {
    let source = virtio_block_source();

    for required in [
        "VirtioInterruptPort",
        "BlockIrqSource::new",
        "impl IrqEndpoint",
        "fn capture(&mut self)",
        "fn contain(",
        "impl IrqSourceControl",
        "fn rearm(&mut self, source: MaskedSource)",
        "IrqCapture::Captured",
        "Arc<mmio_api::Mmio>",
    ] {
        assert!(
            source.contains(required),
            "missing split VirtIO IRQ ownership contract: {required}"
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
        "ioremap_raw",
    ] {
        assert!(
            !source.contains(forbidden),
            "VirtIO IRQ still borrows the controller or exposes deferred acknowledgement: \
             {forbidden}"
        );
    }
}

#[test]
fn discovery_defers_all_virtio_initialization_until_irq_binding() {
    let source = virtio_block_source();

    for required in [
        "ControllerInitEndpoint::Pending",
        "impl<T: VirtIoTransport> rdif_block::InitialController",
        "VirtioBlockInitPhase",
        "InitSchedule::immediate()",
        "take_initialization_source",
    ] {
        assert!(
            source.contains(required),
            "missing staged virtio-blk initialization contract: {required}"
        );
    }

    for forbidden in ["VirtIOBlk::new", "read_consistent("] {
        assert!(
            !source.contains(forbidden),
            "discovery still enters an eager or unbounded upstream initializer: {forbidden}"
        );
    }

    let registration = source
        .find("plat_dev.register_block_with_info")
        .expect("discovery must publish an unresolved block controller");
    let first_init_poll = source
        .find("fn poll_init")
        .expect("initialization must be driven by the runtime endpoint");
    assert!(
        registration < first_init_poll,
        "the discovered controller must be registered before its first init transition exists"
    );
}

#[test]
fn pci_discovery_captures_memory_bars_before_transport_takeover() {
    let source = virtio_block_source();
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
fn mmio_and_pci_preserve_interrupt_capability_before_transport_erasure() {
    let block = virtio_block_source();
    let common = virtio_common_source();

    let pci_port = block
        .find("let interrupt_port = pci_interrupt_port(probe.endpoint())")
        .expect("PCI probe must extract the dedicated ISR capability");
    let pci_transport = block
        .find("crate::pci::take_virtio_block_transport")
        .expect("PCI probe must still construct the queue/config transport");
    assert!(
        pci_port < pci_transport,
        "PCI ISR ownership must be retained before endpoint takeover"
    );

    for required in [
        "VirtioInterruptPort::from_pci_isr",
        "VirtioInterruptPort::from_mmio",
        "register_transport_with_interrupt_port",
        "block::register_mmio_transport",
        "VirtioRegisterMappingLease",
        "if bar >= 6",
        "capability_length < PCI_VIRTIO_ISR_CAP_MIN_LENGTH",
    ] {
        assert!(
            block.contains(required) || common.contains(required),
            "block registration erased its interrupt capability: {required}"
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
fn queue_and_capacity_are_gated_by_the_ready_state() {
    let source = virtio_block_source();

    assert!(source.contains("fn is_ready(&self) -> bool"));
    assert!(source.contains("if self.queue_created || !self.dev.is_ready()"));
    assert!(source.contains("fn capacity_if_ready(&self) -> Option<u64>"));
    assert!(
        source.contains("capacity_if_ready().unwrap_or(0)"),
        "discovery must not publish hardware capacity before init reaches Ready"
    );
}

#[test]
fn recovery_uses_acknowledged_device_reset_and_the_staged_initializer() {
    let source = virtio_block_source();

    for required in [
        "DmaQuiesced::new",
        "ControllerReady::new",
        "finish_reset_after_acknowledgement",
        "prepare_reinitialize",
        "poll_reinitialize",
        "self.queue.take()",
    ] {
        assert!(
            source.contains(required),
            "missing acknowledged VirtIO reset/reinitialize contract: {required}"
        );
    }

    for forbidden in ["ResetUnavailable", "VIRTIO_RESET_UNAVAILABLE"] {
        assert!(
            !source.contains(forbidden),
            "VirtIO recovery still fails closed despite Transport status reset support: \
             {forbidden}"
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
        "queue",
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
        "fn virtio_blk_irq_outcome",
    ] {
        assert!(
            !source.contains(implementation),
            "block/mod.rs mixes implementation responsibility {implementation}"
        );
    }
}
