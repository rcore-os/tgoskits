use std::fs;

use crate::test::qemu::discovery::*;

#[test]
fn discover_qemu_cases_includes_wrapper_root_case() {
    let root = tempfile::tempdir().unwrap();
    let case_dir = root.path().join("suite/root-case");
    fs::create_dir_all(&case_dir).unwrap();
    fs::write(case_dir.join("build-x86_64-unknown-none.toml"), "").unwrap();
    let qemu_config = case_dir.join("qemu-x86_64.toml");
    fs::write(&qemu_config, "").unwrap();

    let cases = discover_qemu_cases(
        &root.path().join("suite"),
        "x86_64",
        "x86_64-unknown-none",
        None,
        "test",
        "qemu",
    )
    .unwrap();

    assert_eq!(cases[0].name, "root-case");
    assert_eq!(cases[0].display_name, "root-case");
    assert_eq!(cases[0].case_dir, case_dir);
    assert_eq!(cases[0].qemu_config_path, qemu_config);
}

#[test]
fn resolve_build_config_ignores_hidden_build_files() {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join(".build-x86_64-unknown-none.toml"),
        "features = []\n",
    )
    .unwrap();
    fs::write(root.path().join(".build-x86_64.toml"), "features = []\n").unwrap();

    assert_eq!(
        resolve_build_config_paths(root.path(), "x86_64-unknown-none").unwrap(),
        []
    );
}

#[test]
fn selected_qemu_case_rejects_path_traversal() {
    let root = tempfile::tempdir().unwrap();
    let build_dir = root.path().join("suite/wrapper");
    fs::create_dir_all(&build_dir).unwrap();
    let build_config = build_dir.join("build-x86_64-unknown-none.toml");
    fs::write(&build_config, "").unwrap();

    let err = discover_qemu_cases(
        root.path().join("suite").as_path(),
        "x86_64",
        "x86_64-unknown-none",
        Some("../escape"),
        "test",
        "qemu",
    )
    .unwrap_err()
    .to_string();

    assert!(err.contains("invalid test qemu test case"));
    assert!(err.contains("path traversal"));
}

#[test]
fn discover_qemu_cases_allow_empty_returns_empty_without_selected_case() {
    let root = tempfile::tempdir().unwrap();
    let case_dir = root.path().join("suite/wrapper/smoke");
    fs::create_dir_all(&case_dir).unwrap();
    fs::write(
        root.path()
            .join("suite/wrapper/build-x86_64-unknown-none.toml"),
        "",
    )
    .unwrap();
    fs::write(case_dir.join("qemu-riscv64.toml"), "").unwrap();

    let cases = discover_qemu_cases_allow_empty(
        root.path().join("suite").as_path(),
        "x86_64",
        "x86_64-unknown-none",
        None,
        "test",
        "qemu",
    )
    .unwrap();

    assert!(cases.is_empty());
}

#[test]
fn discover_qemu_cases_allow_empty_keeps_selected_case_errors() {
    let root = tempfile::tempdir().unwrap();
    let case_dir = root.path().join("suite/wrapper/smoke");
    fs::create_dir_all(&case_dir).unwrap();
    fs::write(
        root.path()
            .join("suite/wrapper/build-x86_64-unknown-none.toml"),
        "",
    )
    .unwrap();
    fs::write(case_dir.join("qemu-riscv64.toml"), "").unwrap();

    let err = discover_qemu_cases_allow_empty(
        root.path().join("suite").as_path(),
        "x86_64",
        "x86_64-unknown-none",
        Some("smoke"),
        "test",
        "qemu",
    )
    .unwrap_err()
    .to_string();

    assert!(err.contains("exists under matching build group"));
    assert!(err.contains("qemu-x86_64.toml"));
}
