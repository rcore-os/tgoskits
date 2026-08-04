#![cfg(all(
    target_os = "linux",
    target_pointer_width = "64",
    target_endian = "little"
))]

use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

const TARGET: &str = "aarch64-unknown-none-softfloat";
const EFI_ENTRY: &str = "<someboot::arch::Arch as someboot::ArchTrait>::efi_enter_kernel";
const EFI_CONTINUATION: &str = "someboot::arch::entry::enter_with_boot_state";
const SETUP_SERVICE: &str = "someboot::efi_stub::setup_service";

#[test]
fn aarch64_efi_handoff_preserves_firmware_state() {
    let temporary_directory = TemporaryDirectory::new();
    let archive = build_aarch64_efi_archive(temporary_directory.path());
    let disassembly = run_output(
        Command::new("rust-objdump")
            .args(["-dr", "-C"])
            .arg(&archive),
        "disassemble the AArch64 EFI someboot archive",
    );
    assert!(
        disassembly.contains("file format elf64-littleaarch64"),
        "the contract must inspect an AArch64 target artifact"
    );

    let efi_entry = function_disassembly(&disassembly, EFI_ENTRY);
    assert_symbol_order(efi_entry, SETUP_SERVICE, EFI_CONTINUATION);
    assert_no_firmware_state_reset(efi_entry, "the AArch64 EFI entry");
    assert!(
        !efi_entry.contains("kernel_entry"),
        "the EFI handoff must not re-enter the direct-boot entry:\n{efi_entry}"
    );

    let continuation = function_disassembly(&disassembly, EFI_CONTINUATION);
    assert!(
        continuation.contains("someboot::arch::elx::switch_to_elx"),
        "the EFI continuation must proceed to exception-level setup:\n{continuation}"
    );
    assert_no_firmware_state_reset(continuation, "the common EFI continuation");

    let setup_service = function_disassembly(&disassembly, SETUP_SERVICE);
    assert_symbol_order(setup_service, "find_fdt", "find_acpi_rsdp");

    let direct_entry = function_disassembly(&disassembly, "kernel_entry");
    for required in [
        "__bss_start",
        "__bss_stop",
        "someboot::fdt::FDT_ADDR",
        EFI_CONTINUATION,
    ] {
        assert!(
            direct_entry.contains(required),
            "the direct-boot entry must retain its {required} initialization:\n{direct_entry}"
        );
    }
}

fn build_aarch64_efi_archive(target_directory: &Path) -> PathBuf {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"));
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace_manifest = manifest_dir
        .join("../..")
        .join("Cargo.toml")
        .canonicalize()
        .expect("workspace manifest must be available");
    run_checked(
        Command::new(cargo)
            .args([
                "build",
                "--quiet",
                "--package",
                "someboot",
                "--lib",
                "--target",
                TARGET,
                "--no-default-features",
                "--features",
                "efi",
                "--target-dir",
            ])
            .arg(target_directory)
            .arg("--manifest-path")
            .arg(workspace_manifest),
        "compile someboot for AArch64 EFI",
    );

    let dependency_directory = target_directory.join(TARGET).join("debug").join("deps");
    let mut archives = fs::read_dir(&dependency_directory)
        .expect("AArch64 dependency directory must be readable")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("libsomeboot-") && name.ends_with(".rlib"))
        })
        .collect::<Vec<_>>();
    assert_eq!(
        archives.len(),
        1,
        "the isolated target directory must contain one someboot archive"
    );
    archives.pop().expect("the someboot archive must exist")
}

fn assert_symbol_order(disassembly: &str, first: &str, second: &str) {
    let first_offset = disassembly
        .find(first)
        .unwrap_or_else(|| panic!("compiled function must reference {first}:\n{disassembly}"));
    let second_offset = disassembly
        .find(second)
        .unwrap_or_else(|| panic!("compiled function must reference {second}:\n{disassembly}"));
    assert!(
        first_offset < second_offset,
        "{first} must execute before {second}:\n{disassembly}"
    );
}

fn assert_no_firmware_state_reset(disassembly: &str, path: &str) {
    for forbidden in ["__bss_start", "__bss_stop", "someboot::fdt::FDT_ADDR"] {
        assert!(
            !disassembly.contains(forbidden),
            "{path} must preserve EFI-populated state instead of referencing \
             {forbidden}:\n{disassembly}"
        );
    }
}

fn function_disassembly<'output>(output: &'output str, symbol: &str) -> &'output str {
    let label = format!("<{symbol}>:");
    let start = output
        .find(&label)
        .unwrap_or_else(|| panic!("compiled archive must define {symbol}"));
    let function = &output[start..];
    function
        .split_once("\n\n")
        .map(|(body, _)| body)
        .unwrap_or(function)
}

fn run_checked(command: &mut Command, operation: &str) {
    let output = command
        .output()
        .unwrap_or_else(|error| panic!("failed to {operation}: {error}"));
    assert!(
        output.status.success(),
        "failed to {operation}:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn run_output(command: &mut Command, operation: &str) -> String {
    let output = command
        .output()
        .unwrap_or_else(|error| panic!("failed to {operation}: {error}"));
    assert!(
        output.status.success(),
        "failed to {operation}:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("tool output must be UTF-8")
}

struct TemporaryDirectory(PathBuf);

impl TemporaryDirectory {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock must follow the Unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "someboot-aarch64-efi-handoff-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("temporary AArch64 target directory must be created");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
