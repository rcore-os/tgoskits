use std::{
    fs,
    path::{Path, PathBuf},
};

use regex::Regex;
use tempfile::tempdir;

use super::{
    app_qemu_test_case, load_qemu_app_case_fields, prepare_qemu_app_case, resolve_qemu_config,
};
use crate::{
    rootfs::qemu::RootfsWritePolicy,
    starry::app::{
        StarryAppQemuCase, discover_apps,
        test_support::{write_case_file, write_test_image_config},
    },
    test::case::HostHttpServerConfig,
};

fn first_step_success_regex(config: &toml::Value) -> Option<&[toml::Value]> {
    config
        .get("shell_check_steps")
        .and_then(toml::Value::as_array)
        .and_then(|steps| steps.first())
        .and_then(|step| step.get("success_regex"))
        .and_then(toml::Value::as_array)
        .map(Vec::as_slice)
}

#[tokio::test]
async fn app_owned_rootfs_runs_declared_builder_without_default_rootfs() {
    let root = tempdir().unwrap();
    write_test_image_config(root.path());
    write_case_file(
        root.path(),
        "nixos",
        "qemu-x86_64.toml",
        r#"args = [
  "-drive",
  "id=disk0,if=none,format=raw,file=${workspace}/.tgos-images/rootfs-x86_64-nixos.img/rootfs-x86_64-nixos.img",
]
uefi = true
to_bin = true
fail_regex = []

[rootfs_preparation]
mode = "app-owned"
builder = "build-rootfs.sh"
target_arch = "x86_64"
"#,
    );
    write_case_file(
        root.path(),
        "nixos",
        "build-rootfs.sh",
        "#!/bin/sh\nset -eu\nprintf 'nixos-image' >\"$STARRY_ROOTFS\"\n",
    );
    let app = discover_apps(root.path())
        .unwrap()
        .into_iter()
        .find(|app| app.name == "nixos")
        .unwrap();

    let case = prepare_qemu_app_case(root.path(), &app, Some("x86_64"), None)
        .await
        .unwrap();

    assert_eq!(fs::read(&case.rootfs_path).unwrap(), b"nixos-image");
    assert!(!root.path().join("tmp/axbuild/rootfs").exists());
}

#[tokio::test]
async fn app_owned_rootfs_rejects_builder_that_does_not_publish_artifact() {
    let root = tempdir().unwrap();
    write_test_image_config(root.path());
    write_case_file(
        root.path(),
        "nixos",
        "qemu-x86_64.toml",
        r#"args = [
  "-drive",
  "id=disk0,if=none,format=raw,file=${workspace}/.tgos-images/rootfs-x86_64-nixos.img/rootfs-x86_64-nixos.img",
]
uefi = true
to_bin = true
fail_regex = []

[rootfs_preparation]
mode = "app-owned"
builder = "build-rootfs.sh"
target_arch = "x86_64"
"#,
    );
    write_case_file(
        root.path(),
        "nixos",
        "build-rootfs.sh",
        "#!/bin/sh\nexit 0\n",
    );
    let app = discover_apps(root.path())
        .unwrap()
        .into_iter()
        .find(|app| app.name == "nixos")
        .unwrap();

    let error = prepare_qemu_app_case(root.path(), &app, Some("x86_64"), None)
        .await
        .unwrap_err()
        .to_string();

    assert!(
        error.contains("did not publish"),
        "unexpected error: {error}"
    );
}

#[tokio::test]
async fn app_owned_rootfs_rejects_target_arch_mismatch_before_builder_runs() {
    let root = tempdir().unwrap();
    write_test_image_config(root.path());
    let builder_marker = root.path().join("builder-ran");
    write_case_file(
        root.path(),
        "nixos",
        "qemu-x86_64.toml",
        r#"args = [
  "-drive",
  "id=disk0,if=none,format=raw,file=${workspace}/.tgos-images/rootfs-x86_64-nixos.img/rootfs-x86_64-nixos.img",
]
uefi = true
to_bin = true
fail_regex = []

[rootfs_preparation]
mode = "app-owned"
builder = "build-rootfs.sh"
target_arch = "aarch64"
"#,
    );
    write_case_file(
        root.path(),
        "nixos",
        "build-rootfs.sh",
        &format!("#!/bin/sh\ntouch '{}'\n", builder_marker.display()),
    );
    let app = discover_apps(root.path())
        .unwrap()
        .into_iter()
        .find(|app| app.name == "nixos")
        .unwrap();

    let error = prepare_qemu_app_case(root.path(), &app, Some("x86_64"), None)
        .await
        .unwrap_err()
        .to_string();

    assert!(
        error.contains("targets `aarch64`"),
        "unexpected error: {error}"
    );
    assert!(!builder_marker.exists());
}

#[test]
fn starrynixos_qemu_matcher_requires_ordered_evidence_and_rejects_failures() {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("axbuild manifest should live under scripts/axbuild")
        .to_path_buf();
    let config_path = repo.join("apps/starry/nixos/qemu-x86_64.toml");
    let config: toml::Value = toml::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();
    let timeout = config
        .get("timeout")
        .and_then(toml::Value::as_integer)
        .unwrap_or_default();
    assert!(
        timeout > 0,
        "{} must classify a partial boot as a timeout instead of waiting indefinitely",
        config_path.display()
    );
    let success_regex = first_step_success_regex(&config)
        .and_then(|patterns| patterns.first())
        .and_then(toml::Value::as_str)
        .map(Regex::new)
        .expect("StarryNixOS must have one compound success regex")
        .unwrap();

    let complete = "STARRY_NIXOS_PHASE=pid1\nSTARRY_NIXOS_PHASE=activation\\
                    nSTARRY_NIXOS_PHASE=systemd\nSTARRY_NIXOS_PHASE=marker\\
                    nSTARRY_NIXOS_SYSTEM_PASSED\n";
    assert!(success_regex.is_match(complete));
    assert!(!success_regex.is_match(
        "STARRY_NIXOS_PHASE=pid1\nSTARRY_NIXOS_PHASE=activation\nSTARRY_NIXOS_SYSTEM_PASSED\n"
    ));
    assert!(!success_regex.is_match(
        "STARRY_NIXOS_PHASE=activation\nSTARRY_NIXOS_PHASE=pid1\nSTARRY_NIXOS_PHASE=systemd\\
         nSTARRY_NIXOS_PHASE=marker\nSTARRY_NIXOS_SYSTEM_PASSED\n"
    ));

    let fail_regexes = config
        .get("fail_regex")
        .and_then(toml::Value::as_array)
        .expect("StarryNixOS must declare terminal failure patterns")
        .iter()
        .map(|pattern| Regex::new(pattern.as_str().unwrap()).unwrap())
        .collect::<Vec<_>>();
    for failure in [
        "kernel panicked at boot",
        "FATAL: PID 1 exited",
        "STARRY_NIXOS_SYSTEM_FAILED: phase=activation",
        "marker.service: Failed with result 'exit-code'",
        "Failed to start Verify the StarryNixOS stage-2 baseline",
    ] {
        assert!(
            fail_regexes.iter().any(|regex| regex.is_match(failure)),
            "failure was not rejected: {failure}"
        );
    }
}

#[test]
fn qemu_config_selection_prefers_exact_arch_config() {
    let root = tempdir().unwrap();
    write_case_file(
        root.path(),
        "codex-cli",
        "qemu-x86_64-codex-help.toml",
        "args = []\n",
    );
    let exact = write_case_file(root.path(), "codex-cli", "qemu-x86_64.toml", "args = []\n");
    let app = discover_apps(root.path())
        .unwrap()
        .into_iter()
        .find(|app| app.name == "codex-cli")
        .unwrap();

    let selected = resolve_qemu_config(&app, Some("x86_64"), None)
        .unwrap()
        .unwrap();

    assert_eq!(selected, exact);
}

#[tokio::test]
async fn qemu_case_uses_starry_default_arch_without_an_arch_argument() {
    let root = tempdir().unwrap();
    write_test_image_config(root.path());
    write_case_file(
        root.path(),
        "qemu/apt",
        "qemu-riscv64.toml",
        r#"args = [
  "-drive",
  "id=disk0,if=none,format=raw,file=${workspace}/.tgos-images/rootfs-riscv64-test.img",
]
uefi = false
to_bin = true
fail_regex = []

[rootfs_preparation]
mode = "app-owned"
builder = "build-rootfs.sh"
target_arch = "riscv64"
"#,
    );
    write_case_file(
        root.path(),
        "qemu/apt",
        "build-rootfs.sh",
        "#!/bin/sh\nset -eu\nprintf 'test-rootfs' >\"$STARRY_ROOTFS\"\n",
    );
    let app = discover_apps(root.path())
        .unwrap()
        .into_iter()
        .find(|app| app.name == "qemu/apt")
        .unwrap();

    let case = prepare_qemu_app_case(root.path(), &app, None, None)
        .await
        .unwrap();

    assert_eq!(case.arch, crate::context::DEFAULT_STARRY_ARCH);
}

#[test]
fn qemu_config_selection_rejects_variant_only_default() {
    let root = tempdir().unwrap();
    write_case_file(
        root.path(),
        "codex-cli",
        "qemu-x86_64-codex-help.toml",
        "args = []\n",
    );
    let app = discover_apps(root.path())
        .unwrap()
        .into_iter()
        .find(|app| app.name == "codex-cli")
        .unwrap();

    let err = resolve_qemu_config(&app, Some("x86_64"), None)
        .unwrap_err()
        .to_string();

    assert!(err.contains("qemu-x86_64.toml"));
}

#[test]
fn qemu_config_selection_uses_explicit_variant_config() {
    let root = tempdir().unwrap();
    let explicit = write_case_file(
        root.path(),
        "codex-cli",
        "qemu-x86_64-codex-syscall-hunt.toml",
        "args = []\n",
    );
    write_case_file(
        root.path(),
        "codex-cli",
        "qemu-x86_64-codex-help.toml",
        "args = []\n",
    );
    let app = discover_apps(root.path())
        .unwrap()
        .into_iter()
        .find(|app| app.name == "codex-cli")
        .unwrap();

    let selected = resolve_qemu_config(
        &app,
        Some("x86_64"),
        Some(Path::new("qemu-x86_64-codex-syscall-hunt.toml")),
    )
    .unwrap()
    .unwrap();

    assert_eq!(selected, explicit);
}

#[test]
fn qemu_case_fields_load_grouped_commands_and_subcases() {
    let root = tempdir().unwrap();
    write_case_file(
        root.path(),
        "qemu/sqlite",
        "qemu-x86_64.toml",
        "args = []\nuefi = false\nto_bin = true\nfail_regex = []\ntest_commands = \
         [\"/usr/bin/app-sqlite\", \"/usr/bin/app-sqlite-deep\"]\n",
    );
    write_case_file(
        root.path(),
        "qemu/sqlite/app-sqlite/c",
        "CMakeLists.txt",
        "cmake_minimum_required(VERSION 3.20)\n",
    );
    write_case_file(
        root.path(),
        "qemu/sqlite/app-sqlite-deep/c",
        "CMakeLists.txt",
        "cmake_minimum_required(VERSION 3.20)\n",
    );
    let app = discover_apps(root.path())
        .unwrap()
        .into_iter()
        .find(|app| app.name == "qemu/sqlite")
        .unwrap();
    let qemu_config = resolve_qemu_config(&app, Some("x86_64"), None).unwrap();

    let fields =
        load_qemu_app_case_fields(root.path(), &app, qemu_config.as_deref().unwrap()).unwrap();

    assert_eq!(
        fields.test_case.test_commands,
        vec!["/usr/bin/app-sqlite", "/usr/bin/app-sqlite-deep"]
    );
    assert_eq!(fields.test_case.subcases.len(), 2);
}

#[test]
fn qemu_case_fields_load_configured_managed_rootfs() {
    let root = tempdir().unwrap();
    write_test_image_config(root.path());
    let rootfs_path = root.path().join(".tgos-images/rootfs-aarch64-debian.img");
    write_case_file(
        root.path(),
        "qemu/apt",
        "qemu-aarch64.toml",
        r#"args = [
  "-drive",
  "id=disk0,if=none,format=raw,file=${workspace}/.tgos-images/rootfs-aarch64-debian.img",
]
uefi = false
to_bin = true
fail_regex = []
"#,
    );
    let app = discover_apps(root.path())
        .unwrap()
        .into_iter()
        .find(|app| app.name == "qemu/apt")
        .unwrap();
    let qemu_config = resolve_qemu_config(&app, Some("aarch64"), None).unwrap();

    let fields =
        load_qemu_app_case_fields(root.path(), &app, qemu_config.as_deref().unwrap()).unwrap();

    assert_eq!(fields.rootfs_path, Some(rootfs_path));
    assert_eq!(fields.write_policy, RootfsWritePolicy::Discard);
}

#[test]
fn qemu_case_fields_load_persistent_rootfs_policy() {
    let root = tempdir().unwrap();
    write_case_file(
        root.path(),
        "macos-selfbuild",
        "qemu-aarch64.toml",
        r#"args = []
uefi = false
to_bin = true
rootfs_write_policy = "persist"
fail_regex = []
"#,
    );
    let app = discover_apps(root.path())
        .unwrap()
        .into_iter()
        .find(|app| app.name == "macos-selfbuild")
        .unwrap();
    let qemu_config = resolve_qemu_config(&app, Some("aarch64"), None).unwrap();

    let fields =
        load_qemu_app_case_fields(root.path(), &app, qemu_config.as_deref().unwrap()).unwrap();

    assert_eq!(fields.write_policy, RootfsWritePolicy::Persist);
}

#[test]
fn app_qemu_test_case_preserves_host_symbolize_success_regex() {
    let case_dir = PathBuf::from("/tmp/apps/starry/memtrack-backtrace");
    let qemu_config_path = case_dir.join("qemu-x86_64.toml");
    let case = StarryAppQemuCase {
        name: "memtrack-backtrace".to_string(),
        arch: "x86_64".to_string(),
        target: "x86_64-unknown-none".to_string(),
        build_config_path: None,
        qemu_config_path: Some(qemu_config_path.clone()),
        rootfs_path: PathBuf::from("/tmp/rootfs.img"),
        rootfs_write_policy: RootfsWritePolicy::Discard,
        test_commands: Vec::new(),
        grouped_command_selection: Default::default(),
        host_symbolize_success_regex: vec!["symbolized".to_string()],
        host_http_server: Some(HostHttpServerConfig {
            bind: "127.0.0.1".to_string(),
            port: 18382,
            body: "fixture".to_string(),
            body_size: None,
            body_byte: b'X',
            dir: None,
        }),
        subcases: Vec::new(),
    };

    let test_case = app_qemu_test_case(&case, case_dir.clone()).unwrap();

    assert_eq!(test_case.case_dir, case_dir);
    assert_eq!(test_case.qemu_config_path, qemu_config_path);
    assert_eq!(test_case.host_symbolize_success_regex, vec!["symbolized"]);
    assert_eq!(
        test_case
            .host_http_server
            .as_ref()
            .map(|config| (config.bind.as_str(), config.port)),
        Some(("127.0.0.1", 18382))
    );
}
