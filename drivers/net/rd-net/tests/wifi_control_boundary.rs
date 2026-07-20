use std::{fs, path::PathBuf};

#[test]
fn wifi_control_is_a_synchronous_owner_fsm_without_runtime_objects() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let source = fs::read_to_string(root.join("src/lib.rs")).expect("read rd-net source");
    let rdif = fs::read_to_string(root.join("../../interface/rdif-eth/src/lib.rs"))
        .expect("read rdif-eth source");

    for required in [
        "pub fn start_wifi_command",
        "pub fn poll_wifi_command",
        "WifiCommandProgress",
        "WifiCommandStartError",
    ] {
        assert!(
            source.contains(required) || rdif.contains(required),
            "missing Wi-Fi owner contract `{required}`"
        );
    }
    for forbidden in ["WaitQueue", "Waker", "spawn", "ax_runtime", "ax_task"] {
        assert!(
            !rdif.contains(forbidden),
            "portable Wi-Fi command contract contains runtime object `{forbidden}`"
        );
    }
}
