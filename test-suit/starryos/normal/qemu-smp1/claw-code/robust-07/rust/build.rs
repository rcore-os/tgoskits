use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let claw_out = out_dir.join("claw-binary");

    // Build claw from source via shared build script (caches result).
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let build_script = manifest_dir.parent().unwrap().parent().unwrap().join("build-claw.sh");

    let output = Command::new("bash")
        .arg(&build_script)
        .output()
        .expect("failed to run build-claw.sh");

    if !output.status.success() {
        panic!(
            "build-claw.sh failed:\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }

    let stdout = String::from_utf8(output.stdout).expect("invalid utf8 from build-claw.sh");
    let claw_bin = stdout.trim();
    assert!(!claw_bin.is_empty(), "build-claw.sh produced no output");

    fs::copy(claw_bin, &claw_out).unwrap_or_else(|e| {
        panic!("failed to copy claw binary from {claw_bin}: {e}")
    });

    println!("cargo:warning=claw built from source: {claw_bin}");
    println!("cargo:rerun-if-changed=build.rs");
}
