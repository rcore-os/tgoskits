use clap::Parser;

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

fn try_parse(args: impl IntoIterator<Item = &'static str>) -> Result<Command, clap::Error> {
    Cli::try_parse_from(args).map(|cli| cli.command)
}

#[test]
fn command_parses_qemu_rootfs_write_policy() {
    match parse([
        "starry",
        "qemu",
        "--arch",
        "aarch64",
        "--rootfs",
        "/tmp/starry-rootfs.img",
        "--rootfs-write-policy",
        "discard",
    ]) {
        Command::Qemu(args) => {
            assert_eq!(
                args.rootfs_write_policy,
                Some(rootfs::RootfsWritePolicy::Discard)
            );
            assert_eq!(
                args.resolved_rootfs_write_policy(),
                rootfs::RootfsWritePolicy::Discard
            );
        }
        _ => panic!("expected qemu command"),
    }
}

#[test]
fn managed_qemu_rootfs_defaults_to_discarding_writes() {
    match parse(["starry", "qemu", "--arch", "aarch64"]) {
        Command::Qemu(args) => assert_eq!(
            args.resolved_rootfs_write_policy(),
            rootfs::RootfsWritePolicy::Discard
        ),
        _ => panic!("expected qemu command"),
    }
}

#[test]
fn explicit_qemu_rootfs_defaults_to_persisting_writes() {
    match parse([
        "starry",
        "qemu",
        "--arch",
        "aarch64",
        "--rootfs",
        "/tmp/starry-rootfs.img",
    ]) {
        Command::Qemu(args) => assert_eq!(
            args.resolved_rootfs_write_policy(),
            rootfs::RootfsWritePolicy::Persist
        ),
        _ => panic!("expected qemu command"),
    }
}

#[test]
fn command_rejects_test_nixos_without_case_selection() {
    assert!(try_parse(["starry", "test", "nixos"]).is_err());
    assert!(try_parse(["starry", "test", "nixos", "--arch", "x86_64"]).is_err());
    assert!(try_parse(["starry", "test", "nixos", "-c", "boot"]).is_err());
}

#[test]
fn command_rejects_unknown_nixos_architecture_but_parses_unknown_case_name() {
    assert!(try_parse(["starry", "test", "nixos", "--arch", "aarch64", "-c", "boot"]).is_err());
    match parse([
        "starry", "test", "nixos", "--arch", "x86_64", "-c", "unknown",
    ]) {
        Command::Test(args) => match args.command {
            TestCommand::Nixos(args) => {
                assert_eq!(args.test_case.as_deref(), Some("unknown"));
            }
            _ => panic!("expected nixos test command"),
        },
        _ => panic!("expected test command"),
    }
}

#[test]
fn command_rejects_app_board_without_case() {
    assert!(Cli::try_parse_from(["starry", "app", "board"]).is_err());
}
