use std::fs;

use tempfile::tempdir;

use super::load_arceos_build_mode;

#[test]
fn app_c_build_config_rejects_missing_source_dir() {
    let root = tempdir().unwrap();
    let path = root.path().join("build-x86_64-unknown-none.toml");
    fs::write(
        &path,
        "app-c = \"missing\"\nfeatures = []\nlog = \"Warn\"\n",
    )
    .unwrap();

    let err = load_arceos_build_mode(&path).unwrap_err();

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
    let path = root.path().join("build-x86_64-unknown-none.toml");
    fs::write(&path, "app-c = \"c\"\nfeatures = []\nlog = \"Warn\"\n").unwrap();

    let err = load_arceos_build_mode(&path).unwrap_err();

    assert!(
        err.to_string()
            .contains("must contain at least one direct .c file"),
        "{err:#}"
    );
}
