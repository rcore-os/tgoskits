use std::path::PathBuf;

use clap::{ArgGroup, Args, Subcommand};

use crate::context::{AxvisorCliArgs, DEFAULT_AXVISOR_ARCH};

/// Axvisor host-side commands
#[derive(Subcommand)]
pub enum Command {
    /// Build Axvisor
    Build(ArgsBuild),
    /// Build and run Axvisor in QEMU
    Qemu(ArgsQemu),
    /// Build and run Axvisor on a remote board
    Board(ArgsBoard),
    /// Run Axvisor test suites
    Test(ArgsTest),
    /// Build and run Axvisor with U-Boot
    Uboot(ArgsUboot),
    /// Generate a default board config
    Defconfig(ArgsDefconfig),
    /// Board config helpers
    Config(ArgsConfig),
    /// Guest image management
    Image(super::image::Args),
}

#[derive(Args, Clone)]
pub struct ArgsBuild {
    #[arg(short, long)]
    pub config: Option<PathBuf>,

    #[arg(long)]
    pub arch: Option<String>,

    #[arg(short, long)]
    pub target: Option<String>,

    #[arg(long = "plat_dyn", alias = "plat-dyn")]
    pub plat_dyn: Option<bool>,

    #[arg(long, value_name = "CPUS")]
    pub smp: Option<usize>,

    #[arg(long)]
    pub debug: bool,

    #[arg(long)]
    pub vmconfigs: Vec<PathBuf>,
}

#[derive(Args)]
pub struct ArgsQemu {
    #[command(flatten)]
    pub build: ArgsBuild,

    #[arg(long)]
    pub qemu_config: Option<PathBuf>,
}

#[derive(Args)]
pub struct ArgsUboot {
    #[command(flatten)]
    pub build: ArgsBuild,

    #[arg(long)]
    pub uboot_config: Option<PathBuf>,
}

#[derive(Args)]
pub struct ArgsBoard {
    #[command(flatten)]
    pub build: ArgsBuild,

    #[arg(long = "board-config")]
    pub board_config: Option<PathBuf>,

    #[arg(short = 'b', long)]
    pub board_type: Option<String>,

    #[arg(long)]
    pub server: Option<String>,

    #[arg(long)]
    pub port: Option<u16>,
}

#[derive(Args)]
pub struct ArgsDefconfig {
    pub board: String,
}

#[derive(Args)]
pub struct ArgsConfig {
    #[command(subcommand)]
    pub command: ConfigCommand,
}

#[derive(Args)]
pub struct ArgsTest {
    #[command(subcommand)]
    pub command: TestCommand,
}

#[derive(Subcommand)]
pub enum TestCommand {
    /// Run Axvisor QEMU test suite
    Qemu(ArgsTestQemu),
    /// Run Axvisor U-Boot board test suite
    Uboot(ArgsTestUboot),
    /// Run Axvisor remote board test suite
    Board(ArgsTestBoard),
    /// Run AxVisor guest test cases
    Cases(ArgsTestCases),
}

#[derive(Args, Debug, Clone)]
pub struct ArgsTestQemu {
    #[arg(long, alias = "arch", value_name = "ARCH")]
    pub target: String,
}

#[derive(Args, Debug, Clone)]
pub struct ArgsTestUboot {
    #[arg(short = 'b', long, value_name = "BOARD")]
    pub board: String,

    #[arg(long, default_value = "linux", value_name = "GUEST")]
    pub guest: String,

    #[arg(long)]
    pub uboot_config: Option<PathBuf>,
}

#[derive(Args, Debug, Clone, Default)]
pub struct ArgsTestBoard {
    #[arg(short = 't', long = "test-group", value_name = "GROUP")]
    pub test_group: Option<String>,

    #[arg(long = "board-test-config")]
    pub board_test_config: Option<PathBuf>,

    #[arg(short = 'b', long = "board-type", value_name = "BOARD_TYPE")]
    pub board_type: Option<String>,

    #[arg(long)]
    pub server: Option<String>,

    #[arg(long)]
    pub port: Option<u16>,
}

#[derive(Args, Debug, Clone)]
#[command(group(
    ArgGroup::new("selection")
        .required(true)
        .args(["suite", "case"])
))]
pub struct ArgsTestCases {
    #[arg(long, value_name = "ARCH", default_value = DEFAULT_AXVISOR_ARCH)]
    pub arch: String,

    #[arg(long, value_name = "SUITE")]
    pub suite: Option<PathBuf>,

    #[arg(long, value_name = "CASE_DIR")]
    pub case: Option<PathBuf>,

    #[arg(long, value_name = "BOOL", num_args = 1)]
    pub guest_log: Option<bool>,
}

#[derive(Subcommand)]
pub enum ConfigCommand {
    /// List available board names
    Ls,
}

impl From<&ArgsBuild> for AxvisorCliArgs {
    fn from(args: &ArgsBuild) -> Self {
        Self {
            config: args.config.clone(),
            arch: args.arch.clone(),
            target: args.target.clone(),
            plat_dyn: args.plat_dyn,
            smp: args.smp,
            debug: args.debug,
            vmconfigs: args.vmconfigs.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[test]
    fn command_parses_uboot() {
        #[derive(clap::Parser)]
        struct Cli {
            #[command(subcommand)]
            command: Command,
        }

        let cli = Cli::try_parse_from([
            "axvisor",
            "uboot",
            "--arch",
            "aarch64",
            "--uboot-config",
            "uboot.toml",
            "--vmconfigs",
            "tmp/vm1.toml",
        ])
        .unwrap();

        match cli.command {
            Command::Uboot(args) => {
                assert_eq!(args.build.arch.as_deref(), Some("aarch64"));
                assert_eq!(args.uboot_config, Some(PathBuf::from("uboot.toml")));
                assert_eq!(args.build.vmconfigs, vec![PathBuf::from("tmp/vm1.toml")]);
            }
            _ => panic!("expected uboot command"),
        }
    }

    #[test]
    fn command_parses_board() {
        #[derive(clap::Parser)]
        struct Cli {
            #[command(subcommand)]
            command: Command,
        }

        let cli = Cli::try_parse_from([
            "axvisor",
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
            "--vmconfigs",
            "tmp/vm1.toml",
        ])
        .unwrap();

        match cli.command {
            Command::Board(args) => {
                assert_eq!(args.build.arch.as_deref(), Some("aarch64"));
                assert_eq!(args.board_config, Some(PathBuf::from("remote.board.toml")));
                assert_eq!(args.board_type.as_deref(), Some("rk3568"));
                assert_eq!(args.server.as_deref(), Some("10.0.0.2"));
                assert_eq!(args.port, Some(9000));
                assert_eq!(args.build.vmconfigs, vec![PathBuf::from("tmp/vm1.toml")]);
            }
            _ => panic!("expected board command"),
        }
    }

    #[test]
    fn command_parses_test_qemu() {
        #[derive(clap::Parser)]
        struct Cli {
            #[command(subcommand)]
            command: Command,
        }

        let cli = Cli::try_parse_from(["axvisor", "test", "qemu", "--arch", "aarch64"]).unwrap();

        match cli.command {
            Command::Test(args) => match args.command {
                TestCommand::Qemu(args) => assert_eq!(args.target, "aarch64"),
                _ => panic!("expected qemu test command"),
            },
            _ => panic!("expected test command"),
        }
    }

    #[test]
    fn command_parses_test_uboot() {
        #[derive(clap::Parser)]
        struct Cli {
            #[command(subcommand)]
            command: Command,
        }

        let cli = Cli::try_parse_from([
            "axvisor",
            "test",
            "uboot",
            "-b",
            "roc-rk3568-pc",
            "--guest",
            "arceos",
            "--uboot-config",
            "uboot.toml",
        ])
        .unwrap();

        match cli.command {
            Command::Test(args) => match args.command {
                TestCommand::Uboot(args) => {
                    assert_eq!(args.board, "roc-rk3568-pc");
                    assert_eq!(args.guest, "arceos");
                    assert_eq!(args.uboot_config, Some(PathBuf::from("uboot.toml")));
                }
                _ => panic!("expected uboot test command"),
            },
            _ => panic!("expected test command"),
        }
    }

    #[test]
    fn command_parses_test_uboot_with_default_guest() {
        #[derive(clap::Parser)]
        struct Cli {
            #[command(subcommand)]
            command: Command,
        }

        let cli = Cli::try_parse_from(["axvisor", "test", "uboot", "-b", "roc-rk3568-pc"]).unwrap();

        match cli.command {
            Command::Test(args) => match args.command {
                TestCommand::Uboot(args) => assert_eq!(args.guest, "linux"),
                _ => panic!("expected uboot test command"),
            },
            _ => panic!("expected test command"),
        }
    }

    #[test]
    fn command_parses_test_board() {
        #[derive(clap::Parser)]
        struct Cli {
            #[command(subcommand)]
            command: Command,
        }

        let cli = Cli::try_parse_from([
            "axvisor",
            "test",
            "board",
            "-t",
            "phytiumpi-linux",
            "-b",
            "Phytiumpi",
            "--board-test-config",
            "board-test.toml",
            "--server",
            "10.0.0.2",
            "--port",
            "9000",
        ])
        .unwrap();

        match cli.command {
            Command::Test(args) => match args.command {
                TestCommand::Board(args) => {
                    assert_eq!(args.test_group.as_deref(), Some("phytiumpi-linux"));
                    assert_eq!(args.board_type.as_deref(), Some("Phytiumpi"));
                    assert_eq!(
                        args.board_test_config,
                        Some(PathBuf::from("board-test.toml"))
                    );
                    assert_eq!(args.server.as_deref(), Some("10.0.0.2"));
                    assert_eq!(args.port, Some(9000));
                }
                _ => panic!("expected board test command"),
            },
            _ => panic!("expected test command"),
        }
    }

    #[test]
    fn command_parses_test_cases_suite() {
        #[derive(clap::Parser)]
        struct Cli {
            #[command(subcommand)]
            command: Command,
        }

        let cli = Cli::try_parse_from([
            "axvisor",
            "test",
            "cases",
            "--arch",
            "aarch64",
            "--suite",
            "test-suit/axvisor/suites/examples.toml",
            "--guest-log",
            "false",
        ])
        .unwrap();

        match cli.command {
            Command::Test(args) => match args.command {
                TestCommand::Cases(args) => {
                    assert_eq!(args.arch, "aarch64");
                    assert_eq!(
                        args.suite,
                        Some(PathBuf::from("test-suit/axvisor/suites/examples.toml"))
                    );
                    assert_eq!(args.case, None);
                    assert_eq!(args.guest_log, Some(false));
                }
                _ => panic!("expected cases test command"),
            },
            _ => panic!("expected test command"),
        }
    }

    #[test]
    fn command_parses_test_cases_case() {
        #[derive(clap::Parser)]
        struct Cli {
            #[command(subcommand)]
            command: Command,
        }

        let cli = Cli::try_parse_from([
            "axvisor",
            "test",
            "cases",
            "--arch",
            "x86_64",
            "--case",
            "test-suit/axvisor/example/pass-report",
        ])
        .unwrap();

        match cli.command {
            Command::Test(args) => match args.command {
                TestCommand::Cases(args) => {
                    assert_eq!(args.arch, "x86_64");
                    assert_eq!(
                        args.case,
                        Some(PathBuf::from("test-suit/axvisor/example/pass-report"))
                    );
                    assert_eq!(args.suite, None);
                    assert_eq!(args.guest_log, None);
                }
                _ => panic!("expected cases test command"),
            },
            _ => panic!("expected test command"),
        }
    }

    #[test]
    fn command_defaults_test_cases_arch_to_aarch64() {
        #[derive(clap::Parser)]
        struct Cli {
            #[command(subcommand)]
            command: Command,
        }

        let cli = Cli::try_parse_from([
            "axvisor",
            "test",
            "cases",
            "--case",
            "test-suit/axvisor/example/fail-report",
        ])
        .unwrap();

        match cli.command {
            Command::Test(args) => match args.command {
                TestCommand::Cases(args) => {
                    assert_eq!(args.arch, "aarch64");
                    assert_eq!(
                        args.case,
                        Some(PathBuf::from("test-suit/axvisor/example/fail-report"))
                    );
                    assert_eq!(args.suite, None);
                }
                _ => panic!("expected cases test command"),
            },
            _ => panic!("expected test command"),
        }
    }

    #[test]
    fn command_rejects_test_cases_without_case_or_suite() {
        #[derive(clap::Parser)]
        struct Cli {
            #[command(subcommand)]
            command: Command,
        }

        assert!(Cli::try_parse_from(["axvisor", "test", "cases"]).is_err());
    }

    #[test]
    fn command_rejects_test_cases_with_case_and_suite() {
        #[derive(clap::Parser)]
        struct Cli {
            #[command(subcommand)]
            command: Command,
        }

        assert!(
            Cli::try_parse_from([
                "axvisor",
                "test",
                "cases",
                "--arch",
                "aarch64",
                "--suite",
                "suite.toml",
                "--case",
                "case-dir",
            ])
            .is_err()
        );
    }

    #[test]
    fn command_parses_build_and_qemu() {
        let build_config = "os/axvisor/.build.toml";
        #[derive(clap::Parser)]
        struct Cli {
            #[command(subcommand)]
            command: Command,
        }

        let build_cli = Cli::try_parse_from([
            "axvisor",
            "build",
            "--config",
            build_config,
            "--arch",
            "aarch64",
            "--vmconfigs",
            "tmp/vm1.toml",
        ])
        .unwrap();
        match build_cli.command {
            Command::Build(args) => {
                assert_eq!(args.config, Some(PathBuf::from(build_config)));
                assert_eq!(args.arch.as_deref(), Some("aarch64"));
                assert_eq!(args.vmconfigs, vec![PathBuf::from("tmp/vm1.toml")]);
            }
            _ => panic!("expected build command"),
        }

        let qemu_cli = Cli::try_parse_from([
            "axvisor",
            "qemu",
            "--config",
            build_config,
            "--arch",
            "aarch64",
            "--qemu-config",
            "configs/qemu.toml",
            "--vmconfigs",
            "tmp/vm1.toml",
            "--vmconfigs",
            "tmp/vm2.toml",
        ])
        .unwrap();
        match qemu_cli.command {
            Command::Qemu(args) => {
                assert_eq!(args.build.config, Some(PathBuf::from(build_config)));
                assert_eq!(args.build.arch.as_deref(), Some("aarch64"));
                assert_eq!(args.qemu_config, Some(PathBuf::from("configs/qemu.toml")));
                assert_eq!(
                    args.build.vmconfigs,
                    vec![PathBuf::from("tmp/vm1.toml"), PathBuf::from("tmp/vm2.toml")]
                );
            }
            _ => panic!("expected qemu command"),
        }
    }
}
