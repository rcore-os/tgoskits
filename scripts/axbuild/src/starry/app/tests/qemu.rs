use std::{fs, path::Path};

use tempfile::tempdir;

use super::{load_qemu_app_case_fields, prepare_qemu_app_case, resolve_qemu_config};
use crate::{
    rootfs::qemu::RootfsWritePolicy,
    starry::app::{
        discover_apps,
        test_support::{write_case_file, write_test_image_config},
    },
};

fn target_dir(root: &Path) -> std::path::PathBuf {
    root.join("custom-target")
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

    let case = prepare_qemu_app_case(
        root.path(),
        &target_dir(root.path()),
        &app,
        Some("x86_64"),
        None,
    )
    .await
    .unwrap();

    assert_eq!(fs::read(&case.rootfs_path).unwrap(), b"nixos-image");
    assert!(!root.path().join("custom-target/axbuild/rootfs").exists());
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

    let error = prepare_qemu_app_case(
        root.path(),
        &target_dir(root.path()),
        &app,
        Some("x86_64"),
        None,
    )
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

    let error = prepare_qemu_app_case(
        root.path(),
        &target_dir(root.path()),
        &app,
        Some("x86_64"),
        None,
    )
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

    let fields = load_qemu_app_case_fields(
        root.path(),
        &target_dir(root.path()),
        &app,
        qemu_config.as_deref().unwrap(),
    )
    .unwrap();

    assert_eq!(
        fields.test_case.test_commands,
        vec!["/usr/bin/app-sqlite", "/usr/bin/app-sqlite-deep"]
    );
    assert_eq!(fields.test_case.subcases.len(), 2);
}
