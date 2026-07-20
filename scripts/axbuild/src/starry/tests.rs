use std::path::{Path, PathBuf};

use clap::Parser;
use ostool::run::qemu::QemuConfig;

use super::*;
use crate::starry::test::TestCommand;

#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

fn parse(args: impl IntoIterator<Item = &'static str>) -> Command {
    Cli::try_parse_from(args).unwrap().command
}

#[test]
fn command_parses_test_qemu() {
    match parse(["starry", "test", "qemu", "--target", "x86_64"]) {
        Command::Test(args) => match args.command {
            TestCommand::Qemu(args) => {
                assert_eq!(args.target.as_deref(), Some("x86_64"));
            }
            _ => panic!("expected qemu test command"),
        },
        _ => panic!("expected test command"),
    }
}

#[test]
fn standard_x86_64_and_loongarch64_qemu_configs_use_uefi_boot() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");

    for arch in ["x86_64", "loongarch64"] {
        let path = workspace.join(format!("os/StarryOS/configs/qemu/qemu-{arch}.toml"));
        let config: QemuConfig = toml::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();

        assert!(config.uefi, "Starry {arch} default QEMU path must use UEFI");
        assert!(
            config.to_bin,
            "Starry {arch} default QEMU path must prepare a BIN"
        );
    }
}

#[test]
fn command_parses_defconfig() {
    match parse(["starry", "defconfig", "qemu-aarch64"]) {
        Command::Defconfig(args) => assert_eq!(args.board, "qemu-aarch64"),
        _ => panic!("expected defconfig command"),
    }
}

#[test]
fn command_parses_config_ls() {
    match parse(["starry", "config", "ls"]) {
        Command::Config(args) => match args.command {
            ConfigCommand::Ls => {}
        },
        _ => panic!("expected config ls command"),
    }
}

#[test]
fn command_parses_test_board() {
    match parse([
        "starry",
        "test",
        "board",
        "-c",
        "smoke",
        "--board",
        "orangepi-5-plus",
        "-b",
        "OrangePi-5-Plus",
        "--server",
        "10.0.0.2",
        "--port",
        "9000",
    ]) {
        Command::Test(args) => match args.command {
            TestCommand::Board(args) => {
                assert_eq!(args.test_case.as_deref(), Some("smoke"));
                assert_eq!(args.board.as_deref(), Some("orangepi-5-plus"));
                assert_eq!(args.board_type.as_deref(), Some("OrangePi-5-Plus"));
                assert_eq!(args.server.as_deref(), Some("10.0.0.2"));
                assert_eq!(args.port, Some(9000));
            }
            _ => panic!("expected board test command"),
        },
        _ => panic!("expected test command"),
    }
}

#[test]
fn command_parses_test_qemu_with_case() {
    match parse([
        "starry",
        "test",
        "qemu",
        "--arch",
        "x86_64",
        "-c",
        "qemu/system",
    ]) {
        Command::Test(args) => match args.command {
            TestCommand::Qemu(args) => {
                assert_eq!(args.arch.as_deref(), Some("x86_64"));
                assert_eq!(args.target, None);
                assert_eq!(args.test_case, Some("qemu/system".to_string()));
            }
            _ => panic!("expected qemu test command"),
        },
        _ => panic!("expected test command"),
    }
}

#[test]
fn command_parses_test_qemu_with_target() {
    match parse(["starry", "test", "qemu", "--target", "x86_64-unknown-none"]) {
        Command::Test(args) => match args.command {
            TestCommand::Qemu(args) => {
                assert_eq!(args.arch, None);
                assert_eq!(args.target.as_deref(), Some("x86_64-unknown-none"));
            }
            _ => panic!("expected qemu test command"),
        },
        _ => panic!("expected test command"),
    }
}

#[test]
fn command_parses_quick_start_qemu_build() {
    match parse(["starry", "quick-start", "qemu-aarch64", "build"]) {
        Command::QuickStart(args) => match args.command {
            quick_start::QuickStartCommand::QemuAarch64(inner) => {
                assert!(matches!(inner.action, quick_start::QuickQemuAction::Build));
            }
            _ => panic!("expected qemu-aarch64 quick-start command"),
        },
        _ => panic!("expected quick-start command"),
    }
}

#[test]
fn command_parses_quick_start_orangepi_run() {
    match parse([
        "starry",
        "quick-start",
        "orangepi-5-plus",
        "run",
        "--serial",
        "/dev/ttyUSB0",
        "--baud",
        "1500000",
    ]) {
        Command::QuickStart(args) => match args.command {
            quick_start::QuickStartCommand::Orangepi5Plus(inner) => match inner.action {
                quick_start::QuickOrangeAction::Run(run) => {
                    assert_eq!(run.serial.as_deref(), Some("/dev/ttyUSB0"));
                    assert_eq!(run.baud.as_deref(), Some("1500000"));
                }
                _ => panic!("expected orangepi run quick-start command"),
            },
            _ => panic!("expected orangepi quick-start command"),
        },
        _ => panic!("expected quick-start command"),
    }
}

#[test]
fn command_parses_quick_start_orangepi_build_with_overrides() {
    match parse([
        "starry",
        "quick-start",
        "orangepi-5-plus",
        "build",
        "--serial",
        "/dev/ttyUSB0",
    ]) {
        Command::QuickStart(args) => match args.command {
            quick_start::QuickStartCommand::Orangepi5Plus(inner) => match inner.action {
                quick_start::QuickOrangeAction::Build(build) => {
                    assert_eq!(build.serial.as_deref(), Some("/dev/ttyUSB0"));
                }
                _ => panic!("expected orangepi build quick-start command"),
            },
            _ => panic!("expected orangepi quick-start command"),
        },
        _ => panic!("expected quick-start command"),
    }
}

#[test]
fn command_parses_quick_start_sg2002_local_run() {
    match parse([
        "starry",
        "quick-start",
        "licheerv-nano-sg2002",
        "run",
        "--serial",
        "/dev/ttyUSB1",
        "--baud",
        "115200",
    ]) {
        Command::QuickStart(args) => match args.command {
            quick_start::QuickStartCommand::LicheervNanoSg2002(inner) => match inner.action {
                quick_start::QuickSg2002Action::Run(run) => {
                    assert_eq!(run.serial.as_deref(), Some("/dev/ttyUSB1"));
                    assert_eq!(run.baud.as_deref(), Some("115200"));
                }
                _ => panic!("expected sg2002 run quick-start command"),
            },
            _ => panic!("expected sg2002 quick-start command"),
        },
        _ => panic!("expected quick-start command"),
    }
}

#[test]
fn formats_starry_app_run_progress() {
    assert_eq!(
        format_app_run_progress(3, 12, "qemu/sqlite", Some("x86_64")),
        "RUN\t3/12\tqemu/sqlite\tarch=x86_64"
    );
    assert_eq!(
        format_app_run_progress(1, 1, "deepseek-tui", None),
        "RUN\t1/1\tdeepseek-tui"
    );
}

#[test]
fn command_parses_app_board() {
    match parse([
        "starry",
        "app",
        "board",
        "-t",
        "orangepi-5-plus-uvc",
        "-b",
        "OrangePi-5-Plus",
        "--server",
        "10.0.0.2",
        "--port",
        "9000",
        "--debug",
    ]) {
        Command::App(args) => match args.command {
            app::AppCommand::Board(args) => {
                assert_eq!(args.test_case, "orangepi-5-plus-uvc");
                assert_eq!(args.board_type.as_deref(), Some("OrangePi-5-Plus"));
                assert_eq!(args.server.as_deref(), Some("10.0.0.2"));
                assert_eq!(args.port, Some(9000));
                assert!(args.debug);
            }
            _ => panic!("expected app board command"),
        },
        _ => panic!("expected app command"),
    }
}

#[test]
fn command_parses_app_board_with_long_case_and_config() {
    match parse([
        "starry",
        "app",
        "board",
        "--test-case",
        "orangepi-5-plus-uvc",
        "--board-config",
        "board.toml",
    ]) {
        Command::App(args) => match args.command {
            app::AppCommand::Board(args) => {
                assert_eq!(args.test_case, "orangepi-5-plus-uvc");
                assert_eq!(args.board_config, Some(PathBuf::from("board.toml")));
            }
            _ => panic!("expected app board command"),
        },
        _ => panic!("expected app command"),
    }
}

#[test]
fn command_parses_app_list() {
    match parse(["starry", "app", "list", "--kind", "qemu"]) {
        Command::App(args) => match args.command {
            app::AppCommand::List(args) => assert_eq!(args.kind, Some(app::StarryAppKind::Qemu)),
            _ => panic!("expected app list command"),
        },
        _ => panic!("expected app command"),
    }
}

#[test]
fn command_parses_app_qemu_all() {
    match parse([
        "starry",
        "app",
        "qemu",
        "--all",
        "--cap",
        "board:OrangePi-5-Plus",
        "--arch",
        "x86_64",
        "--qemu-config",
        "qemu.toml",
        "--debug",
    ]) {
        Command::App(args) => match args.command {
            app::AppCommand::Qemu(args) => {
                assert!(args.all);
                assert_eq!(args.caps, vec!["board:OrangePi-5-Plus"]);
                assert_eq!(args.arch.as_deref(), Some("x86_64"));
                assert_eq!(args.qemu_config, Some(PathBuf::from("qemu.toml")));
                assert!(args.debug);
            }
            _ => panic!("expected app qemu command"),
        },
        _ => panic!("expected app command"),
    }
}

#[test]
fn command_rejects_app_board_without_case() {
    assert!(Cli::try_parse_from(["starry", "app", "board"]).is_err());
}

#[test]
fn command_parses_board() {
    match parse([
        "starry",
        "board",
        "--arch",
        "aarch64",
        "--board-config",
        "remote.board.toml",
        "-b",
        "rk3568",
        "--server",
        "10.0.0.2",
        "--port",
        "9000",
    ]) {
        Command::Board(args) => {
            assert_eq!(args.build.arch.as_deref(), Some("aarch64"));
            assert_eq!(args.board_config, Some(PathBuf::from("remote.board.toml")));
            assert_eq!(args.board_type.as_deref(), Some("rk3568"));
            assert_eq!(args.server.as_deref(), Some("10.0.0.2"));
            assert_eq!(args.port, Some(9000));
        }
        _ => panic!("expected board command"),
    }
}
