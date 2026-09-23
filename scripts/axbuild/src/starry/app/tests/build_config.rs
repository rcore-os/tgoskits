use tempfile::tempdir;

use super::{discover_case_build_config, discover_optional_build_config};
use crate::starry::app::{
    discover_apps,
    test_support::{write_case_file, write_minimal_board_case},
};

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
