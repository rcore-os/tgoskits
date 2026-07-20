use std::{fs, path::Path};

fn source(relative: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(relative);
    fs::read_to_string(&path).unwrap_or_else(|error| panic!("failed to read {path:?}: {error}"))
}

fn function_body<'a>(source: &'a str, signature: &str) -> &'a str {
    let start = source
        .find(signature)
        .unwrap_or_else(|| panic!("missing function {signature}"));
    let open = source[start..]
        .find('{')
        .map(|offset| start + offset)
        .expect("function has no body");
    let mut depth = 0_u32;
    for (offset, byte) in source.as_bytes()[open..].iter().copied().enumerate() {
        match byte {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return &source[start..open + offset + 1];
                }
            }
            _ => {}
        }
    }
    panic!("unterminated function {signature}");
}

fn section<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
    source
        .split_once(start)
        .unwrap_or_else(|| panic!("missing section start {start}"))
        .1
        .split_once(end)
        .unwrap_or_else(|| panic!("missing section end {end}"))
        .0
}

#[test]
fn discovery_only_retains_transport_without_initializing_the_device() {
    let net = source("src/virtio/net.rs");
    let constructor = function_body(&net, "fn new(");
    let make_net = function_body(&net, "fn make_net");

    for body in [constructor, make_net] {
        for forbidden in [
            "VirtIONetRaw::new",
            "begin_init",
            "finish_init",
            "disable_interrupts",
            "enable_interrupts",
        ] {
            assert!(
                !body.contains(forbidden),
                "discovery performs owner-side device work through {forbidden}"
            );
        }
    }
    assert!(constructor.contains("VirtioNetPending"));
}

#[test]
fn owner_initialization_is_bounded_and_generation_checked() {
    let net = source("src/virtio/net.rs");
    let interface = section(
        &net,
        "impl<T: VirtIoTransport> rd_net::Interface",
        "struct VirtioNetIrqEndpoint",
    );
    let poll = function_body(&net, "fn poll_owner_initialization");

    assert!(interface.contains("fn poll_owner_init"));
    for required in [
        "read_config_generation",
        "CONFIG_GENERATION_RETRY_LIMIT",
        "initialization_deadline_ns",
        "OwnerInitSchedule::run_again",
        "OwnerInitSchedule::wait_until",
    ] {
        assert!(
            net.contains(required),
            "missing bounded init proof {required}"
        );
    }
    for forbidden in ["VirtIONetRaw::new", "read_consistent", "loop {", "while "] {
        assert!(
            !poll.contains(forbidden),
            "owner initialization retains an unbounded operation through {forbidden}"
        );
    }
}

#[test]
fn irq_endpoint_is_a_move_only_status_and_ack_capability() {
    let net = source("src/virtio/net.rs");
    let port = section(
        &net,
        "pub struct VirtioNetInterruptPort",
        "struct VirtioNetIrqEndpoint",
    );
    let endpoint = section(
        &net,
        "struct VirtioNetIrqEndpoint",
        "struct VirtioNetPending",
    );

    assert!(!port.contains("derive(Clone"));
    assert!(!port.contains("derive(Copy"));
    assert!(port.contains("capture_status"));
    for forbidden in [
        "SpinNoIrq",
        "OwnerVirtioNetRaw",
        "VirtioNetPending",
        "VirtIoTransport",
        "VirtQueue",
        ".lock()",
    ] {
        assert!(
            !endpoint.contains(forbidden),
            "hard IRQ endpoint reaches owner state through {forbidden}"
        );
    }
}

#[test]
fn device_irq_mask_control_cannot_silently_succeed_before_owner_ready() {
    let net = source("src/virtio/net.rs");
    for signature in ["fn enable_irq", "fn disable_irq"] {
        let control = function_body(&net, signature);
        assert!(
            control.contains("-> Result<(), NetError>"),
            "{signature} cannot report device-side mask failure"
        );
        assert!(
            control.contains("self.ready().ok_or_else(net_not_ready)?"),
            "{signature} silently accepts a device without owner-ready queue state"
        );
    }
}

#[test]
fn event_idx_is_not_negotiated_without_a_complete_source_mask() {
    let net = source("src/virtio/net.rs");
    let supported = section(&net, "const SUPPORTED_FEATURES", "const RESET_RETRY_NS");
    let receive_queue = section(
        &net,
        "VirtioNetInitStage::CreateReceiveQueue",
        "VirtioNetInitStage::CreateTransmitQueue",
    );
    let transmit_queue = section(
        &net,
        "VirtioNetInitStage::CreateTransmitQueue",
        "VirtioNetInitStage::Finish",
    );

    assert!(
        !supported.contains("VIRTIO_F_RING_EVENT_IDX"),
        "set_dev_notify(false) is a no-op with EVENT_IDX and cannot prove used-ring masking"
    );
    for queue in [receive_queue, transmit_queue] {
        assert!(
            queue.contains("false)"),
            "VirtQueue must be constructed in the flag-based notification mode"
        );
        assert!(
            !queue.contains("event_idx"),
            "queue construction still derives an unmaskable EVENT_IDX mode"
        );
    }
}

#[test]
fn used_ring_is_consumed_only_under_a_captured_irq_continuation() {
    let net = source("src/virtio/net.rs");
    assert!(!net.contains("poll_transmit"));
    assert!(!net.contains("poll_receive"));
    assert!(!net.contains("ack_interrupt()"));

    let service = function_body(&net, "fn service_irq_event");
    assert!(service.contains("open_tx_irq_continuation"));
    assert!(service.contains("open_rx_irq_continuation"));

    for (signature, gate, consume) in [
        ("fn reclaim_tx", "tx_irq_continuation", "peek_transmit_used"),
        ("fn reclaim_rx", "rx_irq_continuation", "peek_receive_used"),
    ] {
        let reclaim = function_body(&net, signature);
        let gate = reclaim
            .find(gate)
            .unwrap_or_else(|| panic!("{signature} has no IRQ evidence gate"));
        let used = reclaim
            .find(consume)
            .unwrap_or_else(|| panic!("{signature} does not consume a used descriptor"));
        assert!(
            gate < used,
            "{signature} reads used state before IRQ evidence"
        );
    }
}

#[test]
fn runtime_packet_buffers_remain_cpu_owned_while_virtio_uses_private_rx_dma() {
    let net = source("src/virtio/net.rs");
    let queue_config = function_body(&net, "fn queue_config");
    let submit_rx = function_body(&net, "fn submit_rx");
    let reclaim_rx = function_body(&net, "fn reclaim_rx");

    assert!(
        queue_config.contains("memory_mode: QueueMemoryMode::OwnerCopy"),
        "VirtIO staging queues must tell rd-net not to DMA-sync upper packet buffers"
    );
    assert!(
        submit_rx.contains("staging"),
        "RX submission must create driver-owned DMA staging storage"
    );
    assert!(
        !submit_rx.contains("from_raw_parts_mut(buffer.virt"),
        "RX submission still exposes the upper OwnerCopy buffer to VirtIO DMA"
    );
    assert!(
        reclaim_rx.contains("inflight.staging"),
        "RX completion must reclaim the same driver-owned staging allocation"
    );
    assert!(
        !reclaim_rx.contains("copy_within"),
        "RX completion still mutates a directly DMA-filled upper buffer in place"
    );
}

#[test]
fn pending_ready_and_partial_initialization_own_every_resource_linearly() {
    let net = source("src/virtio/net.rs");
    let virtio = source("src/virtio/mod.rs");
    let take_ready = function_body(&net, "fn take_ready");

    assert!(net.contains("struct VirtioNetPending"));
    assert!(net.contains("impl<T: VirtIoTransport> Drop for VirtioNetInitialization"));
    assert!(net.contains("impl<T: VirtIoTransport> Drop for OwnerVirtioNetRaw"));
    assert!(net.contains("_transport_mapping: Option<Arc<mmio_api::Mmio>>"));
    assert!(virtio.contains("net::register_owned_mmio"));

    let validate_all = take_ready
        .find("self.transport.is_none()")
        .expect("ready transition does not validate every linear owner");
    let move_first_owner = take_ready
        .find("let receive_queue")
        .expect("ready transition does not move queue ownership");
    assert!(
        validate_all < move_first_owner,
        "ready transition can fail after moving only part of its DMA ownership"
    );
    assert!(
        !take_ready[move_first_owner..].contains("ok_or("),
        "ready construction remains fallible after the first owner is moved"
    );

    let static_mmio = function_body(&virtio, "pub fn register_static_mmio");
    let read_device_id = static_mmio
        .find("mapping.read::<u32>(DEVICE_ID_OFFSET)")
        .expect("static MMIO discovery does not inspect the device ID");
    let construct_transport = static_mmio
        .find("probe_mmio_device(mapping.as_ptr(), size)")
        .expect("network branch does not construct its owned transport");
    let register_network = static_mmio
        .find("net::register_owned_mmio")
        .expect("network branch does not transfer mapping ownership");
    assert!(static_mmio.contains("DeviceType::Network as u32"));
    assert!(
        read_device_id < construct_transport && construct_transport < register_network,
        "non-network discovery may construct and reset a VirtIO transport"
    );

    for forbidden in ["Box::leak", "mem::forget", "ioremap_raw"] {
        assert!(
            !net.contains(forbidden),
            "VirtIO net bypasses RAII through {forbidden}"
        );
    }
}
