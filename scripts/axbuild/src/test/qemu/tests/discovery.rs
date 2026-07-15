use std::fs;

use crate::test::qemu::discovery::*;

#[test]
fn discover_all_qemu_cases_includes_wrapper_root_case() {
    let root = tempfile::tempdir().unwrap();
    let case_dir = root.path().join("suite/root-case");
    fs::create_dir_all(&case_dir).unwrap();
    fs::write(case_dir.join("build-x86_64-unknown-none.toml"), "").unwrap();
    fs::write(case_dir.join("qemu-x86_64.toml"), "").unwrap();

    let cases = discover_all_qemu_cases(&root.path().join("suite"), None, "test", "qemu").unwrap();

    assert_eq!(cases, ["root-case"]);
}

#[test]
fn discover_all_qemu_cases_allows_multi_target_wrapper() {
    let root = tempfile::tempdir().unwrap();
    let wrapper_dir = root.path().join("suite/wrapper");
    let case_dir = wrapper_dir.join("case-a");
    fs::create_dir_all(&case_dir).unwrap();
    fs::write(wrapper_dir.join("build-aarch64-unknown-none.toml"), "").unwrap();
    fs::write(wrapper_dir.join("build-x86_64-unknown-none.toml"), "").unwrap();
    fs::write(case_dir.join("qemu-aarch64.toml"), "").unwrap();
    fs::write(case_dir.join("qemu-x86_64.toml"), "").unwrap();

    let cases = discover_all_qemu_cases(&root.path().join("suite"), None, "test", "qemu").unwrap();

    assert_eq!(cases, ["case-a"]);
}

#[test]
fn discover_all_qemu_cases_rejects_unknown_selected_case() {
    let root = tempfile::tempdir().unwrap();
    let case_dir = root.path().join("suite/root-case");
    fs::create_dir_all(&case_dir).unwrap();
    fs::write(case_dir.join("build-x86_64-unknown-none.toml"), "").unwrap();
    fs::write(case_dir.join("qemu-x86_64.toml"), "").unwrap();

    let err = discover_all_qemu_cases(&root.path().join("suite"), Some("missing"), "test", "qemu")
        .unwrap_err()
        .to_string();

    assert!(err.contains("unknown test qemu test case `missing`"));
}

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

    assert_eq!(cases.len(), 1);
    assert_eq!(cases[0].name, "root-case");
    assert_eq!(cases[0].display_name, "root-case");
    assert_eq!(cases[0].case_dir, case_dir);
    assert_eq!(cases[0].qemu_config_path, qemu_config);
}

#[test]
fn discover_qemu_cases_matches_target_variant_configs() {
    let root = tempfile::tempdir().unwrap();
    let wrapper_dir = root.path().join("suite/qemu");
    let case_dir = wrapper_dir.join("smoke");
    fs::create_dir_all(&case_dir).unwrap();
    let svm_build_config = wrapper_dir.join("build-x86_64-unknown-none-svm.toml");
    let vmx_build_config = wrapper_dir.join("build-x86_64-unknown-none-vmx.toml");
    let svm_qemu_config = case_dir.join("qemu-x86_64-svm.toml");
    let vmx_qemu_config = case_dir.join("qemu-x86_64-vmx.toml");
    fs::write(&svm_build_config, "").unwrap();
    fs::write(&vmx_build_config, "").unwrap();
    fs::write(&svm_qemu_config, "").unwrap();
    fs::write(&vmx_qemu_config, "").unwrap();

    let cases = discover_qemu_cases(
        &root.path().join("suite"),
        "x86_64",
        "x86_64-unknown-none",
        None,
        "test",
        "qemu",
    )
    .unwrap();

    assert_eq!(cases.len(), 2);
    assert_eq!(cases[0].name, "smoke-svm");
    assert_eq!(cases[0].display_name, "qemu-svm/smoke-svm");
    assert_eq!(cases[0].qemu_config_path, svm_qemu_config);
    assert_eq!(cases[0].build_config_path, svm_build_config);
    assert_eq!(cases[1].name, "smoke-vmx");
    assert_eq!(cases[1].display_name, "qemu-vmx/smoke-vmx");
    assert_eq!(cases[1].qemu_config_path, vmx_qemu_config);
    assert_eq!(cases[1].build_config_path, vmx_build_config);

    let cases = discover_qemu_cases(
        &root.path().join("suite"),
        "x86_64",
        "x86_64-unknown-none",
        Some("smoke-svm"),
        "test",
        "qemu",
    )
    .unwrap();

    assert_eq!(cases.len(), 1);
    assert_eq!(cases[0].name, "smoke-svm");
    assert_eq!(cases[0].qemu_config_path, svm_qemu_config);
}

#[test]
fn discover_qemu_cases_does_not_duplicate_matching_variant_name() {
    let root = tempfile::tempdir().unwrap();
    let wrapper_dir = root.path().join("suite/qemu");
    let case_dir = wrapper_dir.join("ivc");
    fs::create_dir_all(&case_dir).unwrap();
    let build_config = wrapper_dir.join("build-aarch64-unknown-none-softfloat-ivc.toml");
    let qemu_config = case_dir.join("qemu-aarch64-ivc.toml");
    fs::write(&build_config, "").unwrap();
    fs::write(&qemu_config, "").unwrap();

    let cases = discover_qemu_cases(
        &root.path().join("suite"),
        "aarch64",
        "aarch64-unknown-none-softfloat",
        Some("ivc"),
        "test",
        "qemu",
    )
    .unwrap();

    assert_eq!(cases.len(), 1);
    assert_eq!(cases[0].name, "ivc");
    assert_eq!(cases[0].display_name, "qemu-ivc/ivc");
    assert_eq!(cases[0].qemu_config_path, qemu_config);
    assert_eq!(cases[0].build_config_path, build_config);
}

#[test]
fn resolve_build_config_accepts_target_specific_name_only() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("build-x86_64-unknown-none.toml");
    fs::write(&path, "target = \"x86_64-unknown-none\"\n").unwrap();

    assert_eq!(
        resolve_build_config_paths(root.path(), "x86_64-unknown-none").unwrap(),
        [(None, path)]
    );
}

#[test]
fn resolve_build_config_rejects_legacy_names() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("build-x86_64.toml"), "").unwrap();

    let err = resolve_build_config_paths(root.path(), "x86_64-unknown-none")
        .unwrap_err()
        .to_string();

    assert!(err.contains("unsupported legacy build config name"));
    assert!(err.contains("build-x86_64-unknown-none.toml"));
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
fn selected_qemu_case_allows_same_name_in_later_wrapper() {
    let root = tempfile::tempdir().unwrap();
    let board_dir = root.path().join("suite/board-orangepi-5-plus/smoke");
    fs::create_dir_all(&board_dir).unwrap();
    fs::write(
        root.path()
            .join("suite/board-orangepi-5-plus/build-x86_64-unknown-none.toml"),
        "",
    )
    .unwrap();
    fs::write(board_dir.join("board-orangepi-5-plus.toml"), "").unwrap();

    let qemu_dir = root.path().join("suite/qemu/smoke");
    fs::create_dir_all(&qemu_dir).unwrap();
    fs::write(
        root.path()
            .join("suite/qemu/build-x86_64-unknown-none.toml"),
        "",
    )
    .unwrap();
    let qemu_config = qemu_dir.join("qemu-x86_64.toml");
    fs::write(&qemu_config, "").unwrap();

    let cases = discover_qemu_cases(
        root.path().join("suite").as_path(),
        "x86_64",
        "x86_64-unknown-none",
        Some("smoke"),
        "test",
        "qemu",
    )
    .unwrap();

    assert_eq!(cases.len(), 1);
    assert_eq!(cases[0].build_group, "qemu");
    assert_eq!(cases[0].qemu_config_path, qemu_config);
}

#[test]
fn selected_qemu_case_finds_wrapper_without_scanning_unrelated_broken_tree() {
    let root = tempfile::tempdir().unwrap();
    let target_dir = root.path().join("suite/qemu/smoke");
    fs::create_dir_all(&target_dir).unwrap();
    fs::write(
        root.path()
            .join("suite/qemu/build-x86_64-unknown-none.toml"),
        "",
    )
    .unwrap();
    fs::write(target_dir.join("qemu-x86_64.toml"), "").unwrap();
    fs::create_dir_all(root.path().join("suite/unrelated/broken")).unwrap();

    let cases = discover_qemu_cases(
        root.path().join("suite").as_path(),
        "x86_64",
        "x86_64-unknown-none",
        Some("smoke"),
        "test",
        "qemu",
    )
    .unwrap();

    assert_eq!(cases.len(), 1);
    assert_eq!(cases[0].build_group, "qemu");
}

#[test]
fn selected_qemu_case_reports_existing_case_without_requested_arch_config() {
    let root = tempfile::tempdir().unwrap();
    let target_dir = root.path().join("suite/wrapper/smoke");
    fs::create_dir_all(&target_dir).unwrap();
    fs::write(
        root.path()
            .join("suite/wrapper/build-x86_64-unknown-none.toml"),
        "",
    )
    .unwrap();
    fs::write(target_dir.join("qemu-riscv64.toml"), "").unwrap();

    let err = discover_qemu_cases(
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
