const WRAPPER_SOURCE: &str = include_str!("../src/lib.rs");
const RDIF_SOURCE: &str = include_str!("../src/rdif.rs");

#[test]
fn wrapper_transfers_the_inner_split_irq_source_once() {
    for required in [
        "type IrqEndpoint = sdhci_host::SdhciIrqEndpoint",
        "type IrqControl = sdhci_host::SdhciIrqControl",
        "fn take_irq_source",
        "self.inner.take_irq_source()",
    ] {
        assert!(
            WRAPPER_SOURCE.contains(required),
            "missing split IRQ source contract `{required}`"
        );
    }

    for forbidden in [
        "SdhciIrqHandle",
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
fn rdif_and_portable_wrapper_expose_no_scheduler_policy() {
    for required in ["BIrqEndpoint", "BIrqControl", "QueueExecution"] {
        assert!(
            RDIF_SOURCE.contains(required),
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
            !WRAPPER_SOURCE.contains(forbidden) && !RDIF_SOURCE.contains(forbidden),
            "portable wrapper contains obsolete/runtime policy `{forbidden}`"
        );
    }
}
