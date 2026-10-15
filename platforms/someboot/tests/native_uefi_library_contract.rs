#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

use std::{
    fs,
    path::PathBuf,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

/// This is an artifact contract, not a simulated firmware test. xtask's std
/// runner owns this test; no xtask command builds an isolated someboot library.
#[test]
fn native_uefi_capabilities_do_not_compile_kernel_startup() {
    let temporary = TemporaryDirectory(std::env::temp_dir().join(format!(
        "someboot-native-uefi-{}-{}", std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos(),
    )));
    fs::create_dir(&temporary.0).unwrap();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = Command::new(std::env::var_os("CARGO").unwrap_or("cargo".into()))
        .current_dir(&root)
        .args([
            "build",
            "--package",
            "someboot",
            "--lib",
            "--target",
            "x86_64-unknown-uefi",
            "--no-default-features",
            "--features",
            "efi-handoff",
            "--target-dir",
        ])
        .arg(&temporary.0)
        .output()
        .expect("compile isolated native UEFI library");
    assert!(
        output.status.success(),
        "native UEFI compilation failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    // Cargo's configured build-dir layout may differ from target-dir/deps.
    let mut directories = vec![temporary.0.clone()];
    let mut files = Vec::new();
    while let Some(directory) = directories.pop() {
        for entry in fs::read_dir(directory).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                directories.push(entry.path());
            } else {
                files.push(entry.path());
            }
        }
    }
    let mut archive = None;
    for path in &files {
        let name = path.file_name().unwrap().to_str().unwrap();
        assert!(
            !name.starts_with("libax_cpu-")
                && !name.starts_with("libpage_table_generic-")
                && !name.starts_with("libsome_serial-"),
            "kernel dependency compiled into native UEFI: {name}"
        );
        if name.starts_with("libsomeboot-") && name.ends_with(".rlib") {
            assert!(
                archive.replace(path.clone()).is_none(),
                "ambiguous native UEFI artifact"
            );
        }
    }
    let output = Command::new("rust-nm")
        .args(["--demangle", "--defined-only"])
        .arg(archive.expect("native UEFI library archive"))
        .output()
        .expect("inspect native UEFI symbols");
    assert!(
        output.status.success(),
        "native UEFI symbol inspection failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let symbols = String::from_utf8(output.stdout).unwrap();
    assert!(
        symbols.contains("someboot::handoff::firmware::PreparedHandoff"),
        "artifact must contain the actual EFI capability"
    );
    for forbidden in [
        "someboot::arch::",
        "someboot::smp::",
        "someboot::prime_entry",
        "efi_pe_entry",
        "kernel_entry",
        "__someboot_main",
    ] {
        assert!(
            !symbols.contains(forbidden),
            "kernel startup symbol in native UEFI artifact: {forbidden}"
        );
    }
    for path in &files {
        assert!(
            !matches!(
                path.file_name().and_then(|name| name.to_str()),
                Some("someboot.x" | "defines.rs")
            ),
            "native UEFI must not generate kernel linker configuration: {}",
            path.display()
        );
    }
}
struct TemporaryDirectory(PathBuf);
impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
