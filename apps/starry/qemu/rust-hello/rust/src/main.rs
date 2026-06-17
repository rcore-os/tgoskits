use std::{env, process};

// Embedded from OUT_DIR after build.rs copies the marker written by prebuild.sh.
// When workspace clippy runs without the Starry prebuild flow, build.rs writes
// a placeholder so this crate still compiles.
const PREBUILD_MARKER: &str = include_str!(concat!(env!("OUT_DIR"), "/prebuild_marker.txt"));

fn main() {
    println!("Hello from Rust on StarryOS!");
    println!("PID: {}", process::id());
    println!("Args: {:?}", env::args().collect::<Vec<_>>());

    // Basic sanity checks
    let sum = 1 + 1;
    assert_eq!(sum, 2, "math is broken");
    assert!(process::id() > 0, "getpid failed");

    // Validate that prebuild.sh ran and wrote the marker file.
    let marker = PREBUILD_MARKER.trim();
    assert_eq!(
        marker, "prebuild-ok",
        "prebuild marker mismatch: {marker:?}"
    );
    println!("prebuild marker ok: {marker:?}");

    println!("TEST PASSED");
}
