const RDIF_PRODUCTION_SOURCE: &str = concat!(
    include_str!("../src/bundle.rs"),
    include_str!("../src/evidence.rs"),
    include_str!("../src/error.rs"),
    include_str!("../src/info.rs"),
    include_str!("../src/init.rs"),
    include_str!("../src/interface.rs"),
    include_str!("../src/irq.rs"),
    include_str!("../src/lib.rs"),
    include_str!("../src/lifecycle.rs"),
    include_str!("../src/planner.rs"),
    include_str!("../src/request.rs"),
);

#[test]
fn normal_io_surface_has_no_completion_polling_contract() {
    for forbidden in [
        "fn poll_request",
        "fn poll_completions",
        "RequestPoller",
        "PollOutcome",
        "RequestFlags::POLLED",
        "BlockCompletionMode",
        "irq_driven",
    ] {
        assert!(
            !RDIF_PRODUCTION_SOURCE.contains(forbidden),
            "normal block I/O must not expose polling compatibility `{forbidden}`"
        );
    }
}

#[test]
fn irq_only_surface_keeps_init_deadlines_separate_from_completion_evidence() {
    assert!(RDIF_PRODUCTION_SOURCE.contains("pub const INLINE: Self = Self(usize::MAX)"));
    assert!(RDIF_PRODUCTION_SOURCE.contains("ControllerInitEndpoint"));
    assert!(RDIF_PRODUCTION_SOURCE.contains("wake_at_ns"));
    assert!(RDIF_PRODUCTION_SOURCE.contains("QueueEventBatch"));
    assert!(RDIF_PRODUCTION_SOURCE.contains("PendingBlockIrq"));
    assert!(RDIF_PRODUCTION_SOURCE.contains("DmaQuiesced"));
}
