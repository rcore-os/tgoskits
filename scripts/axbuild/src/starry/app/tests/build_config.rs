use tempfile::tempdir;

use super::{discover_case_build_config, discover_optional_build_config};
use crate::starry::app::{
    discover_apps,
    test_support::{write_case_file, write_minimal_board_case},
};

#[test]
fn reads_build_target_from_filename_when_toml_target_is_absent() {
    let root = tempdir().unwrap();
    write_case_file(root.path(), "demo", "init.sh", "echo hello\n");
    write_case_file(
        root.path(),
        "demo",
        "board-orangepi-5-plus.toml",
        "board_type = \"OrangePi-5-Plus\"\nshell_prefix = \"root@starry:/root #\"\n",
    );
    write_case_file(
        root.path(),
        "demo",
        "build-aarch64-unknown-none-softfloat.toml",
        "env = {}\nfeatures = []\nlog = \"Info\"\n",
    );

    let (_, target) = discover_case_build_config(
        &root.path().join("apps/starry/demo"),
        Some("aarch64-unknown-none-softfloat"),
    )
    .unwrap();

    assert_eq!(target, "aarch64-unknown-none-softfloat");
}

#[test]
fn rejects_mismatched_build_target_filename() {
    let root = tempdir().unwrap();
    write_case_file(root.path(), "demo", "init.sh", "echo hello\n");
    write_case_file(
        root.path(),
        "demo",
        "board-orangepi-5-plus.toml",
        "board_type = \"OrangePi-5-Plus\"\nshell_prefix = \"root@starry:/root #\"\n",
    );
    write_case_file(
        root.path(),
        "demo",
        "build-aarch64-unknown-none-softfloat.toml",
        "target = \"x86_64-unknown-none\"\nenv = {}\nfeatures = []\nlog = \"Info\"\n",
    );

    let err = discover_case_build_config(
        &root.path().join("apps/starry/demo"),
        Some("aarch64-unknown-none-softfloat"),
    )
    .unwrap_err()
    .to_string();

    assert!(err.contains("does not match filename target"));
}

#[test]
fn qemu_build_config_comes_from_app_dir() {
    let root = tempdir().unwrap();
    let build_config = write_case_file(
        root.path(),
        "codex-cli",
        "build-x86_64-unknown-none.toml",
        "target = \"x86_64-unknown-none\"\nfeatures = []\nlog = \"Info\"\n",
    );
    write_case_file(
        root.path(),
        "codex-cli",
        "prebuild.sh",
        "#!/usr/bin/env bash\n",
    );
    write_case_file(root.path(), "codex-cli", "qemu-x86_64.toml", "args = []\n");
    let app = discover_apps(root.path())
        .unwrap()
        .into_iter()
        .find(|app| app.name == "codex-cli")
        .unwrap();

    let selected = discover_optional_build_config(&app.case_dir, "x86_64-unknown-none")
        .unwrap()
        .unwrap();

    assert_eq!(selected, build_config);
}

#[test]
fn qemu_build_config_can_come_from_nearest_parent() {
    let root = tempdir().unwrap();
    let outer = write_case_file(
        root.path(),
        "qemu-smp1",
        "build-x86_64-unknown-none.toml",
        "target = \"x86_64-unknown-none\"\nfeatures = []\nlog = \"Info\"\n",
    );
    let inner = write_case_file(
        root.path(),
        "qemu-smp1/nested",
        "build-x86_64-unknown-none.toml",
        "target = \"x86_64-unknown-none\"\nfeatures = [\"nearest\"]\nlog = \"Info\"\n",
    );
    write_case_file(
        root.path(),
        "qemu-smp1/nested/codex-cli",
        "prebuild.sh",
        "#!/usr/bin/env bash\n",
    );
    write_case_file(
        root.path(),
        "qemu-smp1/nested/codex-cli",
        "qemu-x86_64.toml",
        "args = []\n",
    );
    let app = discover_apps(root.path())
        .unwrap()
        .into_iter()
        .find(|app| app.name == "qemu-smp1/nested/codex-cli")
        .unwrap();

    let selected = discover_optional_build_config(&app.case_dir, "x86_64-unknown-none")
        .unwrap()
        .unwrap();

    assert_eq!(selected, inner);
    assert_ne!(selected, outer);
}

#[test]
fn qemu_build_config_rejects_legacy_arch_name() {
    let root = tempdir().unwrap();
    write_case_file(
        root.path(),
        "codex-cli",
        "build-x86_64.toml",
        "target = \"x86_64-unknown-none\"\nfeatures = []\nlog = \"Info\"\n",
    );
    write_case_file(
        root.path(),
        "codex-cli",
        "prebuild.sh",
        "#!/usr/bin/env bash\n",
    );
    write_case_file(root.path(), "codex-cli", "qemu-x86_64.toml", "args = []\n");
    let app = discover_apps(root.path())
        .unwrap()
        .into_iter()
        .find(|app| app.name == "codex-cli")
        .unwrap();

    let err = discover_optional_build_config(&app.case_dir, "x86_64-unknown-none")
        .unwrap_err()
        .to_string();

    assert!(err.contains("unsupported legacy build config name"));
    assert!(err.contains("build-x86_64.toml"));
}

#[test]
fn board_case_still_accepts_minimal_build_config() {
    let root = tempdir().unwrap();
    write_minimal_board_case(root.path(), "demo");

    let (path, target) =
        discover_case_build_config(&root.path().join("apps/starry/demo"), None).unwrap();

    assert!(path.ends_with("build-aarch64-unknown-none-softfloat.toml"));
    assert_eq!(target, "aarch64-unknown-none-softfloat");
}
