//! Source contracts for one CPU-pinned USB host maintenance owner.

const USB_IRQ: &str = include_str!("../src/pseudofs/usbfs/irq.rs");
const USB_MANAGER: &str = include_str!("../src/pseudofs/usbfs/manager.rs");
const USB_GLUE: &str = include_str!("../../../../drivers/ax-driver/src/usb/mod.rs");
const USB_EHCI: &str =
    include_str!("../../../../drivers/usb/usb-host/src/backend/kmod/ehci/mod.rs");

#[test]
fn each_hardware_host_has_one_fixed_maintenance_owner() {
    for required in [
        "spawn_maintenance_domain",
        "MaintenanceRegistrar",
        "MaintenanceThread",
        "registrar.register_shared_disabled",
        "MaintenanceIrqAction",
        "LocalIrqWake",
        "install_maintenance_thread",
    ] {
        assert!(
            USB_IRQ.contains(required),
            "USB host is missing fixed-owner contract `{required}`"
        );
    }

    for forbidden in [
        "usbfs_event_service_task",
        "usbfs_poll_ticker_task",
        "USBFS_EVENT_WORKER_STARTED",
        "USBFS_POLL_TICKER_STARTED",
        "USBFS_EVENT_POLL_INTERVAL",
        "IrqNotify",
        "handler_busy",
        "deferred",
    ] {
        assert!(
            !USB_IRQ.contains(forbidden),
            "USB host still uses split ownership or polling through `{forbidden}`"
        );
    }
}

#[test]
fn hard_irq_only_captures_acknowledges_and_wakes_the_local_owner() {
    let irq_action = function_body(USB_IRQ, "fn usb_irq_action(");

    assert!(irq_action.contains("capture"));
    assert!(irq_action.contains("publish_from_irq"));
    assert!(irq_action.contains("IrqReturn::Wake"));
    for forbidden in [
        ".handle()",
        "handle_event",
        "notify_usb_activity",
        "probe_devices",
        "poll_request",
        "service_host_events",
        "lock()",
    ] {
        assert!(
            !irq_action.contains(forbidden),
            "hard IRQ still advances USB state through `{forbidden}`"
        );
    }
}

#[test]
fn initialization_and_event_progress_run_in_the_owner_loop() {
    let owner = function_body(USB_IRQ, "fn usb_owner_loop(");

    assert!(owner.contains("initialize_host"));
    assert!(owner.contains("service_host_events"));
    assert!(owner.contains("drain_owner"));
    assert!(owner.contains("USBFS_EVENT_BATCH_LIMIT"));

    for forbidden in [
        "guard.host_mut().init()",
        "guard.host_mut().probe_devices()",
        "enable_device_irq",
        "bootstrap_device",
    ] {
        assert!(
            !USB_MANAGER.contains(forbidden),
            "USB manager still accesses owner-only host state through `{forbidden}`"
        );
    }
}

#[test]
fn live_devices_endpoints_and_transfer_queues_remain_owner_local() {
    for required in [
        "UsbDeviceId",
        "UsbEndpointId",
        "UsbTransferTicket",
        "service_transfer_completions",
    ] {
        assert!(
            USB_IRQ.contains(required),
            "USB owner facade is missing typed capability `{required}`"
        );
    }

    for forbidden in [
        "PiMutex<Device>",
        "Mutex<Endpoint>",
        ".submit(",
        ".cancel(",
        ".reclaim(",
        ".poll_request(",
        "ctrl_ep_mut",
    ] {
        assert!(
            !USB_MANAGER.contains(forbidden),
            "USBFS manager still accesses an owner-only queue through `{forbidden}`"
        );
    }
}

#[test]
fn hardware_hosts_without_an_irq_fail_closed() {
    assert!(USB_MANAGER.contains("MissingIrq"));
    assert!(
        !USB_IRQ.contains("slot.irq.is_none()"),
        "hardware host must not fall back to a periodic event poller"
    );
    assert!(
        !USB_IRQ.contains("polling event handler"),
        "hardware host without an IRQ must not be published as polling"
    );
}

#[test]
fn teardown_is_explicit_and_retains_failed_irq_ownership() {
    let close = function_body(USB_IRQ, "fn close_usb_maintenance(");

    assert_ordered(
        close,
        &[
            "begin_close",
            "disable",
            "synchronize",
            "close()",
            "try_begin_draining",
            "finish_close",
        ],
    );
    assert!(close.contains("quarantine_and_park"));
    assert!(!close.contains("free_irq"));
    assert!(!close.contains("mem::forget"));
    assert!(!close.contains("Box::leak"));
}

#[test]
fn portable_usb_driver_exposes_event_capture_without_runtime_policy() {
    assert!(USB_GLUE.contains("capture_irq"));
    assert!(USB_GLUE.contains("contain"));
    for forbidden in [
        "spawn_maintenance_domain",
        "LocalIrqWake",
        "queue_work_on",
        "ThreadWakeHandle",
    ] {
        assert!(
            !USB_GLUE.contains(forbidden),
            "portable USB glue must not depend on runtime policy `{forbidden}`"
        );
    }
}

#[test]
fn ehci_waiters_are_woken_by_irq_service_instead_of_self_polling() {
    assert!(USB_EHCI.contains("service_ehci_event"));
    assert!(USB_EHCI.contains("waiter.wake()"));
    assert!(
        !USB_EHCI.contains("cx.waker().wake_by_ref()"),
        "EHCI endpoint futures must not turn queue completion into self polling"
    );
}

fn function_body<'a>(source: &'a str, signature: &str) -> &'a str {
    let start = source
        .find(signature)
        .unwrap_or_else(|| panic!("missing function `{signature}`"));
    let tail = &source[start..];
    let open = tail.find('{').expect("function must have a body");
    let mut depth = 0usize;
    for (offset, byte) in tail[open..].bytes().enumerate() {
        match byte {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return &tail[..open + offset + 1];
                }
            }
            _ => {}
        }
    }
    panic!("unterminated function `{signature}`")
}

fn assert_ordered(source: &str, needles: &[&str]) {
    let mut cursor = 0usize;
    for needle in needles {
        let offset = source[cursor..]
            .find(needle)
            .unwrap_or_else(|| panic!("missing ordered step `{needle}`"));
        cursor += offset + needle.len();
    }
}
