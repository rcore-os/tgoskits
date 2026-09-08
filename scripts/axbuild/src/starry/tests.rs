use clap::Parser;

use super::*;

#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

fn parse(args: impl IntoIterator<Item = &'static str>) -> Command {
    Cli::try_parse_from(args).unwrap().command
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
