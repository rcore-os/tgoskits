use std::{fs, path::Path, process::Command};

use ostool::{board::config::BoardRunConfig, run::ShellCheckStep};
use tempfile::tempdir;

use super::{configure_board_init_step, merge_board_init_command, resolve_board_case};
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
fn board_and_app_init_execute_as_one_serial_shell_command() {
    let command = merge_board_init_command(
        "printf \"app:%s\\n\" \"$APP_BOARD_VALUE\"",
        Some("export APP_BOARD_VALUE='board value'\nprintf \"prelude\\n\""),
    );

    assert!(
        !command.contains('\n'),
        "interactive shell command must not expose script line boundaries"
    );
    let output = Command::new("sh").arg("-c").arg(command).output().unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"prelude\napp:board value\n");
}

#[test]
fn board_shell_prelude_is_injected_before_the_shared_init_script() {
    let mut board = BoardRunConfig {
        board_type: "test".to_string(),
        shell_check_steps: vec![ShellCheckStep {
            shell_prefix: Some("root@starry:".to_string()),
            shell_cmd: Some(
                "export BLOCK_RW_BENCH_CONTROLLER='custom-controller'\nexport \
                 BLOCK_RW_BENCH_SUCCESS_MARKER='CUSTOM_BLOCK_RW_BENCH_PASSED'"
                    .to_string(),
            ),
            success_regex: Some(vec!["PASSED".to_string()]),
            ..Default::default()
        }],
        fail_regex: vec!["panic".to_string()],
        timeout: Some(90),
        ..Default::default()
    };

    configure_board_init_step(&mut board, "echo hello").unwrap();

    assert_eq!(
        board.shell_check_steps[0].shell_cmd.as_deref(),
        Some(
            merge_board_init_command(
                "echo hello",
                Some(
                    "export BLOCK_RW_BENCH_CONTROLLER='custom-controller'\nexport \
                     BLOCK_RW_BENCH_SUCCESS_MARKER='CUSTOM_BLOCK_RW_BENCH_PASSED'"
                )
            )
            .as_str()
        )
    );
    assert_eq!(board.fail_regex, vec!["panic"]);
    assert_eq!(board.timeout, Some(90));
}

#[test]
fn passive_board_step_receives_init_command() {
    let mut board = BoardRunConfig {
        board_type: "test".to_string(),
        shell_check_steps: vec![ShellCheckStep {
            shell_prefix: Some("root@starry:#".into()),
            success_regex: Some(vec!["PASSED".to_string()]),
            ..Default::default()
        }],
        ..Default::default()
    };

    configure_board_init_step(&mut board, "echo hello").unwrap();

    assert_eq!(
        board.shell_check_steps[0].shell_prefix.as_deref(),
        Some("root@starry:#")
    );
    assert_eq!(
        board.shell_check_steps[0].shell_cmd.as_deref(),
        Some("printf %b 'echo hello' | sh")
    );
    assert_eq!(
        board.shell_check_steps[0].success_regex.as_deref(),
        Some(&["PASSED".to_string()][..])
    );
}

#[test]
fn board_without_shell_check_step_is_rejected() {
    let mut board = BoardRunConfig {
        board_type: "test".to_string(),
        ..Default::default()
    };
    let error = configure_board_init_step(&mut board, "echo hello").unwrap_err();
    assert!(error.to_string().contains("shell_check_steps"));
}

#[test]
fn board_init_step_rejects_ambiguous_or_unmatchable_config() {
    let step = ShellCheckStep {
        shell_prefix: Some("root#".into()),
        shell_cmd: Some("prelude".into()),
        ..Default::default()
    };
    let mut multiple = BoardRunConfig {
        board_type: "test".into(),
        shell_check_steps: vec![step.clone(), step.clone()],
        ..Default::default()
    };
    assert!(configure_board_init_step(&mut multiple, "app").is_err());

    let mut missing_prefix = BoardRunConfig {
        board_type: "test".into(),
        shell_check_steps: vec![ShellCheckStep {
            shell_prefix: None,
            ..step
        }],
        ..Default::default()
    };
    assert!(configure_board_init_step(&mut missing_prefix, "app").is_err());
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
    let board_build = write_board_default(
        root.path(),
        "orangepi-5-plus",
        "aarch64-unknown-none-softfloat",
    );

    let case = resolve_board_case(root.path(), "demo", None).unwrap();

    assert_eq!(case.target, "aarch64-unknown-none-softfloat");
    assert_eq!(case.build_config_path, board_build);
}

#[test]
fn board_default_build_config_is_used_without_an_app_override() {
    let root = tempdir().unwrap();
    write_case_file(root.path(), "demo", "init.sh", "echo hello\n");
    write_case_file(
        root.path(),
        "demo",
        "board-visionfive2.toml",
        "board_type = \"VisionFive2\"\nshell_prefix = \"root@starry:\"\n",
    );
    let board_build = write_board_default(root.path(), "visionfive2", "riscv64gc-unknown-none-elf");

    let case = resolve_board_case(root.path(), "demo", None).unwrap();

    assert_eq!(case.target, "riscv64gc-unknown-none-elf");
    assert_eq!(case.build_config_path, board_build);
}
