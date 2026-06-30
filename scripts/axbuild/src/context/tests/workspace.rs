use super::common::*;
use crate::context::workspace::workspace_root_path_from;

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
