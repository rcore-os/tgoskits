use std::fs;

use tempfile::tempdir;

use super::resolve_app_c_dir;

#[test]
fn app_c_build_config_rejects_missing_source_dir() {
    let root = tempdir().unwrap();
    let missing = root.path().join("missing");
    let err = resolve_app_c_dir(&missing).unwrap_err();

    assert!(
        err.to_string().contains("app-c source directory"),
        "{err:#}"
    );
}

#[test]
fn app_c_build_config_rejects_source_dir_without_c_files() {
    let root = tempdir().unwrap();
    let source_dir = root.path().join("c");
    fs::create_dir_all(&source_dir).unwrap();
    fs::write(source_dir.join("main.rs"), "fn main() {}\n").unwrap();
    let err = resolve_app_c_dir(&source_dir).unwrap_err();

    assert!(
        err.to_string()
            .contains("must contain at least one direct .c file"),
        "{err:#}"
    );
}

#[test]
fn app_c_source_dir_must_be_absolute() {
    let err = resolve_app_c_dir(std::path::Path::new("apps/arceos/example/c")).unwrap_err();

    assert!(err.to_string().contains("must be absolute"), "{err:#}");
}
