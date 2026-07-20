const DMA_SERVICE: &str = include_str!("../src/dma/service.rs");
const DMA_TYPES: &str = include_str!("../src/dma/mod.rs");

#[test]
fn dma_completion_has_no_boolean_quiescence_or_anonymous_leak_path() {
    let source = [DMA_SERVICE, DMA_TYPES].concat();

    for forbidden in [
        "finish_block_request_with_quiesce",
        "quiesced: bool",
        "core::mem::forget",
        "abort(true, false)",
        "abort(false, false)",
    ] {
        assert!(
            !source.contains(forbidden),
            "SDHCI DMA ownership still contains `{forbidden}`"
        );
    }
}
