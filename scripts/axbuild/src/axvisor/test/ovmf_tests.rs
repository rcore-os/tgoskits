use std::{fs, path::Path, process::Command};

use sha2::{Digest, Sha256};
use tempfile::tempdir;

const CODE_SIZE: usize = 0x37c000;
const VARS_SIZE: usize = 0x84000;

#[test]
fn ovmf_bundle_verifier_accepts_the_fixed_profile() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let script = workspace_root.join("os/axvisor/scripts/ovmf-profile.sh");
    let bundle = tempdir().unwrap();
    write_valid_bundle(bundle.path());

    let status = run_verifier(&script, bundle.path());

    assert!(status.success());
}

#[test]
fn ovmf_bundle_verifier_rejects_changed_code_content() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let script = workspace_root.join("os/axvisor/scripts/ovmf-profile.sh");
    let bundle = tempdir().unwrap();
    write_valid_bundle(bundle.path());
    let code_path = bundle.path().join("OVMF_CODE.fd");
    let mut code = fs::read(&code_path).unwrap();
    code[0] ^= 1;
    fs::write(code_path, code).unwrap();

    let status = run_verifier(&script, bundle.path());

    assert!(!status.success());
}

#[test]
fn ovmf_bundle_verifier_rejects_changed_code_size() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let script = workspace_root.join("os/axvisor/scripts/ovmf-profile.sh");
    let bundle = tempdir().unwrap();
    write_valid_bundle(bundle.path());
    let code_path = bundle.path().join("OVMF_CODE.fd");
    let mut code = fs::read(&code_path).unwrap();
    code.pop();
    fs::write(code_path, code).unwrap();

    let status = run_verifier(&script, bundle.path());

    assert!(!status.success());
}

#[test]
fn ovmf_bundle_verifier_rejects_changed_code_base() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let script = workspace_root.join("os/axvisor/scripts/ovmf-profile.sh");
    let bundle = tempdir().unwrap();
    write_valid_bundle(bundle.path());
    let manifest_path = bundle.path().join("manifest.toml");
    let manifest = fs::read_to_string(&manifest_path)
        .unwrap()
        .replace("code_base = 0xffc84000", "code_base = 0xffc00000");
    fs::write(manifest_path, manifest).unwrap();

    let status = run_verifier(&script, bundle.path());

    assert!(!status.success());
}

#[test]
fn ovmf_bundle_verifier_rejects_duplicate_manifest_keys() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let script = workspace_root.join("os/axvisor/scripts/ovmf-profile.sh");

    for duplicate in [
        "profile = \"ignored-duplicate\"",
        "code_base = 0xffc00000",
        "code_sha256 = \"ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff\"",
    ] {
        let bundle = tempdir().unwrap();
        write_valid_bundle(bundle.path());
        let manifest_path = bundle.path().join("manifest.toml");
        let mut manifest = fs::read_to_string(&manifest_path).unwrap();
        manifest.push_str(duplicate);
        manifest.push('\n');
        fs::write(manifest_path, manifest).unwrap();

        let status = run_verifier(&script, bundle.path());

        assert!(!status.success(), "accepted duplicate key: {duplicate}");
    }
}

#[test]
fn ovmf_bundle_verifier_rejects_fields_inside_a_table() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let script = workspace_root.join("os/axvisor/scripts/ovmf-profile.sh");
    let bundle = tempdir().unwrap();
    write_valid_bundle(bundle.path());
    let manifest_path = bundle.path().join("manifest.toml");
    let manifest = format!(
        "[metadata]\n{}",
        fs::read_to_string(&manifest_path).unwrap()
    );
    fs::write(manifest_path, manifest).unwrap();

    let status = run_verifier(&script, bundle.path());

    assert!(!status.success());
}

#[test]
fn ovmf_bundle_verifier_rejects_quoted_keys() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let script = workspace_root.join("os/axvisor/scripts/ovmf-profile.sh");
    let bundle = tempdir().unwrap();
    write_valid_bundle(bundle.path());
    let manifest_path = bundle.path().join("manifest.toml");
    let mut manifest = fs::read_to_string(&manifest_path).unwrap();
    manifest.push_str("\"profile\" = \"ignored-duplicate\"\n");
    fs::write(manifest_path, manifest).unwrap();

    let status = run_verifier(&script, bundle.path());

    assert!(!status.success());
}

#[test]
fn external_firmware_requires_explicit_unverified_opt_in() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let script = workspace_root.join("os/axvisor/scripts/ovmf-profile.sh");
    let firmware_dir = tempdir().unwrap();
    let firmware = firmware_dir.path().join("OVMF_CODE.fd");
    fs::write(&firmware, vec![0xa5; CODE_SIZE]).unwrap();

    let rejected = run_prepare_firmware(&script, &firmware, None);
    let accepted = run_prepare_firmware(&script, &firmware, Some("1"));

    assert!(!rejected.success());
    assert!(accepted.success());
}

fn run_verifier(script: &Path, bundle: &Path) -> std::process::ExitStatus {
    Command::new("bash")
        .args([
            "-c",
            "set -euo pipefail; source \"$1\"; ovmf_verify_bundle \"$2\"",
            "ovmf-profile-test",
        ])
        .arg(script)
        .arg(bundle)
        .status()
        .unwrap()
}

fn run_prepare_firmware(
    script: &Path,
    firmware: &Path,
    allow_unverified: Option<&str>,
) -> std::process::ExitStatus {
    let mut command = Command::new("bash");
    command
        .args([
            "-c",
            "set -euo pipefail; source \"$1\"; ovmf_prepare_firmware /unused /unused",
            "ovmf-profile-test",
        ])
        .arg(script)
        .env("AXVISOR_X86_64_UEFI_FIRMWARE", firmware)
        .env_remove("AXVISOR_X86_64_UEFI_ALLOW_UNVERIFIED");
    if let Some(value) = allow_unverified {
        command.env("AXVISOR_X86_64_UEFI_ALLOW_UNVERIFIED", value);
    }
    command.status().unwrap()
}

fn write_valid_bundle(bundle: &Path) {
    let vars = vec![0x5a; VARS_SIZE];
    let code = vec![0xa5; CODE_SIZE];
    let mut combined = Vec::with_capacity(VARS_SIZE + CODE_SIZE);
    combined.extend_from_slice(&vars);
    combined.extend_from_slice(&code);

    fs::write(bundle.join("OVMF_VARS.fd"), &vars).unwrap();
    fs::write(bundle.join("OVMF_CODE.fd"), &code).unwrap();
    fs::write(bundle.join("OVMF.fd"), &combined).unwrap();
    fs::write(
        bundle.join("manifest.toml"),
        format!(
            r#"schema_version = 1
profile = "qemu_x86_64_axvisor_ovmf_debug"
edk2_tag = "edk2-stable202605"
edk2_commit = "b03a21a63e3bd001f52c527e5a57feddb53a690b"
architecture = "X64"
target = "DEBUG"
toolchain = "GCC"
platform = "OvmfPkg/OvmfPkgX64.dsc"
build_command = "build -a X64 -b DEBUG"
build_container_digest = "sha256:fixture"
tool_versions = "fixture"
submodule_commits = "fixture"
code_base = 0xffc84000
code_size = 0x37c000
vars_base = 0xffc00000
vars_size = 0x84000
combined_size = 0x400000
reset_vector = 0xfffffff0
code_file = "OVMF_CODE.fd"
code_sha256 = "{}"
vars_file = "OVMF_VARS.fd"
vars_sha256 = "{}"
combined_file = "OVMF.fd"
combined_sha256 = "{}"
fd_size_4mb = true
debug_on_serial_port = true
build_shell = true
smm_require = false
secure_boot_enable = false
tpm2_enable = false
network_enable = false
sdcard_enable = false
cc_measurement_enable = false
sec_marker = "SecCoreStartupWithStack("
pei_marker = "Platform PEIM Loaded"
dxe_ipl_marker = "DXE IPL Entry"
dxe_core_marker = "Loading DXE CORE at"
bds_marker = "[BdsDxe]"
"#,
            sha256(&code),
            sha256(&vars),
            sha256(&combined),
        ),
    )
    .unwrap();
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
