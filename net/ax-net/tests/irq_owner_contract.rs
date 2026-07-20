//! Cross-layer source contracts for fixed-owner Ethernet IRQ maintenance.

const ETHERNET: &str = include_str!("../src/device/ethernet.rs");
const DRIVER: &str = include_str!("../src/device/driver.rs");
const ROUTER: &str = include_str!("../src/router.rs");
const AX_NET: &str = include_str!("../src/lib.rs");
const RUNTIME_NET: &str = include_str!("../../../os/arceos/modules/axruntime/src/net.rs");
const RDIF_ETH: &str = include_str!("../../../drivers/interface/rdif-eth/src/lib.rs");
const RD_NET: &str = include_str!("../../../drivers/net/rd-net/src/lib.rs");
const FXMAC: &str = include_str!("../../../drivers/net/fxmac_rs/src/fxmac.rs");
const FXMAC_IRQ: &str = include_str!("../../../drivers/net/fxmac_rs/src/fxmac_intr.rs");
const FXMAC_UTILS: &str = include_str!("../../../drivers/net/fxmac_rs/src/utils.rs");

#[test]
fn ax_net_exposes_a_device_boundary_instead_of_registering_irqs() {
    assert!(!ETHERNET.contains("EthernetIrqRegistrar"));
    assert!(!ETHERNET.contains("register_shared("));
    assert!(!ETHERNET.contains("use polling"));
    assert!(!DRIVER.contains("irq_framework"));
    assert!(!DRIVER.contains("rd_net::"));
    assert!(DRIVER.contains("runtime facade"));
    assert!(DRIVER.contains("fn readiness_poll"));
    assert!(ROUTER.contains("fn device_worker"));
    assert!(!ROUTER.contains("fn device_rx_worker"));
    assert!(!ROUTER.contains("fn device_tx_worker"));
    assert!(!ROUTER.contains("DEVICE_RX_IDLE_POLL_INTERVAL"));
    assert!(!AX_NET.contains("wake_net_task_irq"));
    assert!(!AX_NET.contains("device_poll_fallback_due"));
    assert!(!AX_NET.contains("NET_POLL_REQUESTED"));
    assert!(!AX_NET.contains("publish_poll_request"));
    assert!(!AX_NET.contains("take_poll_request"));
    assert!(!AX_NET.contains("IDLE_POLL_INTERVAL"));
}

#[test]
fn hard_irq_capture_and_task_context_service_are_separate_capabilities() {
    assert!(RDIF_ETH.contains("BIrqEndpoint"));
    assert!(RDIF_ETH.contains("IrqEndpoint<Event = Event, Fault = EthernetIrqFault>"));
    assert!(RDIF_ETH.contains("fn service_irq_event"));
    assert!(RDIF_ETH.contains("fn rearm_irq_source"));

    let action = function_body(RUNTIME_NET, "fn net_irq_action");
    assert!(action.contains("capture_irq"));
    assert!(action.contains("publish_from_irq"));
    assert!(!action.contains("service_irq_event"));
    assert!(!action.contains("PollSet"));
}

#[test]
fn portable_net_owner_is_linear_and_does_not_leak_the_controller() {
    assert!(!RD_NET.contains("UnsafeCell"));
    assert!(!RD_NET.contains("unsafe impl Sync for NetInner"));
    assert!(!RD_NET.contains("WifiControlHandle"));
    assert!(!FXMAC.contains("Box::leak"));
    assert!(!FXMAC_IRQ.contains("AtomicPtr<FXmac>"));
    assert!(!FXMAC_UTILS.contains("Box::into_raw"));
    assert!(FXMAC.contains("pub unsafe fn discover_xmac"));
    assert!(FXMAC.contains("pub fn begin_xmac_init(pending: FXmacPending)"));
    assert!(FXMAC.contains("pub fn poll_xmac_init"));
}

#[test]
fn runtime_owns_the_fixed_thread_registration_and_local_wake() {
    for required in [
        "spawn_maintenance_domain",
        "registrar.register_shared_disabled",
        "MaintenanceIrqAction",
        "LocalIrqWake",
        "MaintenanceThread",
    ] {
        assert!(
            RUNTIME_NET.contains(required),
            "network runtime misses owner contract `{required}`"
        );
    }
    assert!(RUNTIME_NET.contains("activate_net_device"));
    assert!(RUNTIME_NET.contains("RuntimeEthernetDriver"));
    assert!(!RUNTIME_NET.contains("queue_work_on"));
    assert!(!RUNTIME_NET.contains("wait_timeout"));

    let owner = function_body(RUNTIME_NET, "fn run_net_owner");
    let prepare = owner
        .find("prepare_net_irq_owner")
        .expect("owner must first register its disabled IRQ action");
    let mask = owner
        .find("net.disable_irq()")
        .expect("owner must mask the device before enabling its IRQ action");
    assert!(
        prepare < mask,
        "portable discovery must not access device MMIO before the final owner has registered a \
         disabled action"
    );

    let take_endpoint = function_body(RD_NET, "pub fn take_irq_endpoint");
    for forbidden in ["irq_guard", "is_irq_enabled", "disable_irq", "enable_irq"] {
        assert!(
            !take_endpoint.contains(forbidden),
            "moving the IRQ endpoint must not touch hardware through `{forbidden}`"
        );
    }
}

#[test]
fn queue_completion_requires_captured_irq_evidence() {
    let prepare_send = function_body(RD_NET, "pub fn prepare_send");
    let try_submit = function_body(RD_NET, "pub fn try_submit");
    assert!(
        !prepare_send.contains("reclaim_bounded"),
        "TX admission must not discover completion without an IRQ event"
    );
    assert!(
        !try_submit.contains("reclaim_bounded"),
        "TX submit must not discover completion without an IRQ event"
    );

    let owner_loop = function_body(RUNTIME_NET, "fn net_owner_loop");
    assert!(owner_loop.contains("tx_irq_pending"));
    assert!(owner_loop.contains("rx_irq_pending"));
    assert!(owner_loop.contains("if rx_irq_pending"));

    let try_receive = function_body(RD_NET, "pub fn try_receive");
    assert!(
        !try_receive.contains("Ok(None) | Err(_)"),
        "RX corruption must reach the maintenance owner instead of looking drained"
    );
}

#[test]
fn remote_submission_does_not_hold_ingress_lock_across_owner_wake() {
    let submit = function_body(RUNTIME_NET, "fn submit_packet");
    let unlock = submit
        .find("drop(ingress)")
        .expect("submission must release ingress ownership explicitly");
    let activate = submit
        .find("publish_cause")
        .expect("submission must activate its maintenance owner");
    assert!(
        unlock < activate,
        "scheduler wake ran while ingress lock was held"
    );
    assert!(!submit.contains("pop_back"));
}

#[test]
fn runtime_does_not_drop_dma_queues_without_a_quiesce_proof() {
    let owner = function_body(RUNTIME_NET, "fn run_net_owner");
    assert!(owner.contains("activate_queues"));
    assert!(owner.contains("quarantine_and_park"));
    assert!(!RUNTIME_NET.contains("fn close_net_owner"));
}

fn function_body<'a>(source: &'a str, signature: &str) -> &'a str {
    let start = source
        .find(signature)
        .unwrap_or_else(|| panic!("missing method `{signature}`"));
    let tail = &source[start..];
    let end = tail.find("\n}\n").map_or(tail.len(), |offset| offset + 2);
    &tail[..end]
}
