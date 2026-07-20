const WRAPPER_SOURCE: &str = include_str!("../src/lib.rs");

#[test]
fn wrapper_transfers_inner_split_irq_source_once() {
    for required in [
        "type IrqEndpoint = DwMmcIrqEndpoint",
        "type IrqControl = DwMmcIrqControl",
        "fn take_irq_source",
        "self.inner.take_irq_source()",
    ] {
        assert!(
            WRAPPER_SOURCE.contains(required),
            "missing split IRQ source contract `{required}`"
        );
    }

    for forbidden in [
        "DwMmcIrq,",
        "type IrqHandle",
        "fn irq_handle",
        ".irq_endpoint()",
        "SdioIrqHandle",
    ] {
        assert!(
            !WRAPPER_SOURCE.contains(forbidden),
            "legacy IRQ wrapper escape hatch `{forbidden}` remains"
        );
    }
}

#[test]
fn portable_wrapper_exposes_no_scheduler_policy_or_old_rdif_contract() {
    for required in ["BIrqEndpoint", "BIrqControl", "QueueExecution"] {
        assert!(
            WRAPPER_SOURCE.contains(required),
            "RDIF wrapper is missing `{required}`"
        );
    }
    for forbidden in [
        "BIrqHandler",
        "DispatchMode",
        "ax_task",
        "workqueue",
        "queue_work_on",
        "spawn_maintenance",
    ] {
        assert!(
            !WRAPPER_SOURCE.contains(forbidden),
            "portable wrapper contains obsolete/runtime policy `{forbidden}`"
        );
    }
}
