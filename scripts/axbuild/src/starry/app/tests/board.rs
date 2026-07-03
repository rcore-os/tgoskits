use std::{fs, path::Path};

use tempfile::tempdir;

use super::resolve_board_case;
use crate::starry::app::test_support::{
    write_board_default, write_case_file, write_minimal_board_case,
};

#[test]
fn resolves_board_case_from_apps_dir() {
    let root = tempdir().unwrap();
    write_minimal_board_case(root.path(), "demo");

    let case = resolve_board_case(root.path(), "demo", None).unwrap();

    assert_eq!(case.name, "demo");
    assert_eq!(case.target, "aarch64-unknown-none-softfloat");
    assert_eq!(case.init_cmd, "echo hello");
    assert!(
        case.board_config_path
            .ends_with("board-orangepi-5-plus.toml")
    );
    assert!(
        case.build_config_path
            .ends_with("build-aarch64-unknown-none-softfloat.toml")
    );
}

#[test]
fn reports_missing_apps_dir() {
    let root = tempdir().unwrap();

    let err = resolve_board_case(root.path(), "demo", None)
        .unwrap_err()
        .to_string();

    assert!(err.contains("missing Starry apps directory"));
    assert!(err.contains("apps/starry"));
}

#[test]
fn reports_unknown_case_with_available_cases() {
    let root = tempdir().unwrap();
    write_minimal_board_case(root.path(), "demo");

    let err = resolve_board_case(root.path(), "missing", None)
        .unwrap_err()
        .to_string();

    assert!(err.contains("unknown Starry app case `missing`"));
    assert!(err.contains("demo"));
}

#[test]
fn explicit_board_config_overrides_case_config() {
    let root = tempdir().unwrap();
    write_minimal_board_case(root.path(), "demo");
    let explicit = root.path().join("custom-board.toml");
    fs::write(&explicit, "board_type = \"custom\"\n").unwrap();

    let case = resolve_board_case(root.path(), "demo", Some(explicit.as_path())).unwrap();

    assert_eq!(case.board_config_path, explicit);
}

#[test]
fn explicit_relative_board_config_can_resolve_inside_case() {
    let root = tempdir().unwrap();
    write_minimal_board_case(root.path(), "demo");
    let explicit = write_case_file(
        root.path(),
        "demo",
        "board-custom.toml",
        "board_type = \"Custom\"\nshell_prefix = \"root@starry:/root #\"\n",
    );

    let case =
        resolve_board_case(root.path(), "demo", Some(Path::new("board-custom.toml"))).unwrap();

    assert_eq!(case.board_config_path, explicit);
}

#[test]
fn board_default_target_picks_matching_build_config() {
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
        "target = \"aarch64-unknown-none-softfloat\"\nenv = {}\nfeatures = []\nlog = \"Info\"\n",
    );
    write_case_file(
        root.path(),
        "demo",
        "build-riscv64gc-unknown-none-elf.toml",
        "target = \"riscv64gc-unknown-none-elf\"\nenv = {}\nfeatures = []\nlog = \"Info\"\n",
    );
    write_board_default(
        root.path(),
        "orangepi-5-plus",
        "aarch64-unknown-none-softfloat",
    );

    let case = resolve_board_case(root.path(), "demo", None).unwrap();

    assert_eq!(case.target, "aarch64-unknown-none-softfloat");
}
