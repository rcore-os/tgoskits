use std::path::PathBuf;

use clap::{Args, Subcommand};

#[derive(Args)]
pub struct ArgsTest {
    #[command(subcommand)]
    pub command: TestCommand,
}

#[derive(Subcommand)]
pub enum TestCommand {
    /// Run StarryOS QEMU test suite
    Qemu(ArgsTestQemu),
    /// Run StarryOS remote board test suite
    Board(ArgsTestBoard),
    /// Run StarryOS-backed NixOS tests
    Nixos(ArgsTestNixos),
}

#[derive(Args, Debug, Clone)]
pub struct ArgsTestQemu {
    #[arg(
        long,
        value_name = "ARCH",
        required_unless_present_any = ["target", "list", "build_config"],
        conflicts_with = "build_config",
        help = "StarryOS architecture to test"
    )]
    pub arch: Option<String>,
    #[arg(
        short = 't',
        long,
        value_name = "TARGET",
        required_unless_present_any = ["arch", "list", "build_config"],
        conflicts_with = "build_config",
        help = "StarryOS target triple to test"
    )]
    pub target: Option<String>,
    #[arg(
        short = 'c',
        long = "test-case",
        value_name = "CASE",
        help = "Run only one StarryOS QEMU test case"
    )]
    pub test_case: Option<String>,
    #[arg(short = 'l', long, help = "List discovered StarryOS QEMU test cases")]
    pub list: bool,

    #[arg(
        long,
        value_name = "PATH",
        requires_all = ["qemu_config", "rootfs"],
        conflicts_with_all = ["test_case", "list"],
        help = "Run once with an external Starry build config"
    )]
    pub build_config: Option<PathBuf>,

    #[arg(
        long,
        value_name = "PATH",
        requires_all = ["build_config", "rootfs"],
        conflicts_with_all = ["test_case", "list"],
        help = "External QEMU config for a one-shot run"
    )]
    pub qemu_config: Option<PathBuf>,

    #[arg(
        long,
        value_name = "IMAGE",
        requires_all = ["build_config", "qemu_config"],
        conflicts_with_all = ["test_case", "list"],
        help = "Prepared rootfs image for a one-shot run"
    )]
    pub rootfs: Option<PathBuf>,

    #[arg(
        long,
        value_name = "ELF",
        requires = "build_config",
        help = "Boot this validated, byte-preserved ELF instead of the fresh build"
    )]
    pub fixed_elf: Option<PathBuf>,
}

#[derive(Args, Debug, Clone, Default)]
pub struct ArgsTestBoard {
    #[arg(
        short = 'c',
        long = "test-case",
        value_name = "CASE",
        help = "Run only one Starry board test case"
    )]
    pub test_case: Option<String>,

    #[arg(
        long,
        value_name = "BOARD",
        help = "Run all Starry board test cases for one board"
    )]
    pub board: Option<String>,

    #[arg(short = 'b', long = "board-type", value_name = "BOARD_TYPE")]
    pub board_type: Option<String>,

    #[arg(long)]
    pub server: Option<String>,

    #[arg(long)]
    pub port: Option<u16>,

    #[arg(short = 'l', long, help = "List discovered Starry board test cases")]
    pub list: bool,
}

#[derive(Args, Debug, Clone, Default)]
pub struct ArgsTestNixos {
    #[arg(
        long,
        value_name = "ARCH",
        value_parser = ["x86_64"],
        required_unless_present = "list",
        requires = "test_case",
        conflicts_with = "list",
        help = "StarryOS architecture to test"
    )]
    pub arch: Option<String>,

    #[arg(
        short = 'c',
        long = "test-case",
        value_name = "CASE",
        required_unless_present = "list",
        requires = "arch",
        conflicts_with = "list",
        help = "Run one StarryOS-backed NixOS test case"
    )]
    pub test_case: Option<String>,

    #[arg(short = 'l', long, help = "List StarryOS-backed NixOS test cases")]
    pub list: bool,
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::ArgsTestQemu;

    #[derive(Parser)]
    struct TestCli {
        #[command(flatten)]
        qemu: ArgsTestQemu,
    }

    #[test]
    fn external_run_accepts_complete_inputs_without_arch_selection() {
        let cli = TestCli::try_parse_from([
            "test",
            "--build-config",
            "build.toml",
            "--qemu-config",
            "qemu.toml",
            "--rootfs",
            "rootfs.img",
            "--fixed-elf",
            "/tmp/starryos",
        ])
        .unwrap();

        assert!(cli.qemu.arch.is_none());
        assert!(cli.qemu.target.is_none());
    }

    #[test]
    fn external_run_rejects_partial_or_discovery_inputs() {
        assert!(
            TestCli::try_parse_from([
                "test",
                "--build-config",
                "build.toml",
                "--rootfs",
                "rootfs.img",
            ])
            .is_err()
        );
        assert!(
            TestCli::try_parse_from([
                "test",
                "--arch",
                "x86_64",
                "--build-config",
                "build.toml",
                "--qemu-config",
                "qemu.toml",
                "--rootfs",
                "rootfs.img",
            ])
            .is_err()
        );
        assert!(
            TestCli::try_parse_from([
                "test",
                "--build-config",
                "build.toml",
                "--qemu-config",
                "qemu.toml",
                "--rootfs",
                "rootfs.img",
                "--list",
            ])
            .is_err()
        );
    }
}
