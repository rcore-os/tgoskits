use std::{fs, path::PathBuf};

fn kernel_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn loop_block_is_a_synchronous_filesystem_device_without_polling_runtime() {
    let source = fs::read_to_string(kernel_root().join("src/pseudofs/dev/loop_block.rs")).unwrap();

    assert!(source.contains("impl BlockDevice for LoopBlockDevice"));
    for forbidden in [
        "IQueue",
        "poll_request",
        "RequestStatus",
        "BlockIrqBridge",
        "BlockDrainWake",
    ] {
        assert!(
            !source.contains(forbidden),
            "loop block retained obsolete polling runtime token {forbidden}"
        );
    }
}
