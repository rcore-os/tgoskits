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

#[test]
fn virtio_block_uses_interrupt_owned_request_contract() {
    let source = virtio_block_source();

    for required in [
        "QueueKind::Interrupt",
        "DispatchMode::Direct",
        "submit_owned",
        "service_events",
        "QueueEventBatch",
        "fn shutdown",
        "CompletedRequest",
    ] {
        assert!(
            source.contains(required),
            "missing rdif-block 0.12 contract: {required}"
        );
    }

    for forbidden in ["poll_request", "RequestStatus", "next_request_id"] {
        assert!(
            !source.contains(forbidden),
            "legacy polling/driver-ID contract remains: {forbidden}"
        );
    }
}

#[test]
fn irq_lock_contention_uses_a_typed_deferred_ack_continuation() {
    let source = virtio_block_source();
    let contention = source
        .find("irq_ack_pending.swap(true, Ordering::AcqRel)")
        .expect("IRQ contention must retain pending acknowledgement state");
    let reset = source[contention..]
        .find("irq_ack_pending.store(false, Ordering::Release)")
        .map(|offset| contention + offset)
        .expect("successful IRQ ownership must clear deferred acknowledgement state");
    let contention_path = &source[contention..reset];

    assert!(
        contention_path.contains("Event::deferred_from_queue_bits")
            && contention_path.contains("IrqOutcome::deferred"),
        "IRQ contention must explicitly defer acknowledgement instead of fabricating an +         \
         acknowledged completion event"
    );
    assert!(source.contains("take_deferred_virtio_queue_irq"));
    assert!(source.contains("events.requires_irq_ack()"));
    assert!(source.contains("try_with_task"));
    assert!(source.contains("unwrap_or(Ok(ServiceProgress::More))"));
}

#[test]
fn discovery_defers_all_virtio_initialization_until_irq_binding() {
    let source = virtio_block_source();

    for required in [
        "ControllerInitEndpoint::Pending",
        "impl<T: VirtIoTransport> rdif_block::InitialController",
        "VirtioBlockInitPhase",
        "InitSchedule::immediate()",
        "initialization_irq_outcome",
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
        "descriptor_storage",
        "core::mem::forget(queue)",
        "core::mem::forget(inflight)",
        "core::mem::forget(storage)",
    ] {
        assert!(
            source.contains(required),
            "VirtIO init failure can release live DMA without {required}"
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
