use std::process::Command;

use super::common::*;
use crate::context::{WorkspaceContext, workspace::workspace_root_path_from};

const CARGO_TARGET_DIR_CHILD: &str = "AXBUILD_TEST_CARGO_TARGET_DIR_CHILD";

#[test]
fn workspace_context_uses_metadata_target_directory() {
    let root = tempdir().unwrap();
    let app = test_app_context(root.path());

    assert_eq!(app.target_dir(), root.path().join("target"));
}

#[test]
fn workspace_context_respects_cargo_config_target_directory() {
    let root = tempdir().unwrap();
    let _ = test_app_context(root.path());
    fs::create_dir_all(root.path().join(".cargo")).unwrap();
    fs::write(
        root.path().join(".cargo/config.toml"),
        "[build]\ntarget-dir = \"configured-target\"\n",
    )
    .unwrap();

    let workspace = WorkspaceContext::from_root(root.path(), None).unwrap();

    assert_eq!(
        workspace.target_dir(),
        root.path().join("configured-target")
    );
}

#[test]
fn workspace_context_respects_relative_cargo_target_dir_environment() {
    if std::env::var_os(CARGO_TARGET_DIR_CHILD).is_some() {
        let root = tempdir().unwrap();
        let workspace = test_app_context(root.path());
        assert_eq!(
            workspace.target_dir(),
            root.path().join("environment-target")
        );
        return;
    }

    let status = Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg(
            "context::tests::workspace::workspace_context_respects_relative_cargo_target_dir_environment",
        )
        .env(CARGO_TARGET_DIR_CHILD, "1")
        .env("CARGO_TARGET_DIR", "environment-target")
        .status()
        .unwrap();
    assert!(status.success());
}

#[test]
fn explicit_target_directory_overrides_metadata_and_is_workspace_relative() {
    let root = tempdir().unwrap();
    let _ = test_app_context(root.path());
    fs::create_dir_all(root.path().join(".cargo")).unwrap();
    fs::write(
        root.path().join(".cargo/config.toml"),
        "[build]\ntarget-dir = \"configured-target\"\n",
    )
    .unwrap();

    let relative =
        WorkspaceContext::from_root(root.path(), Some(Path::new("explicit-target"))).unwrap();
    assert_eq!(relative.target_dir(), root.path().join("explicit-target"));

    let absolute_target = root.path().join("absolute-target");
    let absolute =
        WorkspaceContext::from_root(root.path(), Some(absolute_target.as_path())).unwrap();
    assert_eq!(absolute.target_dir(), absolute_target);
}

#[test]
fn debug_mode_keeps_the_selected_target_directory() {
    let root = tempdir().unwrap();
    let mut app = test_app_context(root.path());
    let target_dir = app.target_dir().to_path_buf();

    app.set_debug_mode(true).unwrap();

    assert_eq!(app.target_dir(), target_dir);
}

#[test]
fn workspace_root_path_uses_runtime_workspace_when_compile_time_path_is_unavailable() {
    let root = tempdir().unwrap();
    let nested = root.path().join("scripts/axbuild/src");
    fs::create_dir_all(&nested).unwrap();
    fs::write(
        root.path().join("Cargo.toml"),
        "[workspace]\nmembers = [\"scripts/axbuild\"]\n",
    )
    .unwrap();

    let missing_compile_manifest_dir = root.path().join("missing/scripts/axbuild");

    let resolved = workspace_root_path_from(&nested, &missing_compile_manifest_dir).unwrap();

    assert_eq!(resolved, root.path().canonicalize().unwrap());
}
