const RAMDISK_SOURCE: &str = include_str!("../src/lib.rs");
const RAMDISK_MANIFEST: &str = include_str!("../Cargo.toml");

#[test]
fn inline_ramdisk_has_no_async_completion_or_os_lock_state() {
    for forbidden in [
        "ax_kspin",
        "AtomicBool",
        "AtomicU64",
        "RamIrqState",
        "RamIrqHandler",
        "completed: Vec<RequestId>",
        "next_req_id",
        "fn poll_request",
        "fn poll_completions",
    ] {
        assert!(
            !RAMDISK_SOURCE.contains(forbidden),
            "inline ramdisk must not retain asynchronous state `{forbidden}`"
        );
    }
    assert!(!RAMDISK_MANIFEST.contains("ax-kspin"));
}

#[test]
fn inline_ramdisk_returns_terminal_ownership_in_the_submit_call() {
    assert!(RAMDISK_SOURCE.contains("InlineBlockDevice"));
    assert!(RAMDISK_SOURCE.contains("impl InlineExecuteQueue for RamDiskInlineQueue"));
    assert!(!RAMDISK_SOURCE.contains("QueueExecution::Tagged"));
    assert!(!RAMDISK_SOURCE.contains("QueueExecution::Serialized"));
    assert!(RAMDISK_SOURCE.contains("RequestId::INLINE"));
    assert!(RAMDISK_SOURCE.contains("CompletedRequest::new"));
    assert!(RAMDISK_SOURCE.contains("let storage = self.storage.take()?"));
}

#[test]
fn inline_ramdisk_has_no_hardware_lifecycle_surface() {
    for forbidden in [
        "impl IQueue",
        "impl Interface",
        "LifecycleEndpoint",
        "fn shutdown(",
        "fn service_events(",
        "fn reclaim_after_quiesce(",
        "QueueEventBatch",
        "SubmitOutcome",
    ] {
        assert!(
            !RAMDISK_SOURCE.contains(forbidden),
            "inline ramdisk retained hardware lifecycle surface `{forbidden}`"
        );
    }
}
