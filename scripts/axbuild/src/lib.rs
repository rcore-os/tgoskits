#![cfg_attr(not(any(windows, unix)), no_std)]
#![cfg(any(windows, unix))]

#[macro_use]
extern crate log;

#[macro_use]
extern crate anyhow;

use clap::{Parser, Subcommand};

use crate::{arceos::ArceOS, axvisor::Axvisor, starry::Starry};

pub mod arceos;
pub mod axvisor;
pub mod context;
mod download;
mod logging;
pub mod process;
pub mod starry;
mod test_qemu;
mod test_std;

#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Workspace test commands
    Test {
        #[command(subcommand)]
        command: TestCommand,
    },
    /// Axvisor host-side commands
    Axvisor {
        #[command(subcommand)]
        command: axvisor::Command,
    },
    /// ArceOS build commands
    Arceos {
        #[command(subcommand)]
        command: arceos::Command,
    },
    /// StarryOS build commands
    Starry {
        #[command(subcommand)]
        command: starry::Command,
    },
}

#[derive(Subcommand)]
enum TestCommand {
    /// Run std tests for the configured workspace package whitelist
    Std,
    /// Run QEMU test suites
    Qemu {
        #[command(subcommand)]
        command: QemuTestCommand,
    },
    /// Run U-Boot test suites
    Uboot {
        #[command(subcommand)]
        command: UbootTestCommand,
    },
}

#[derive(Subcommand)]
enum QemuTestCommand {
    /// Run ArceOS QEMU test suites
    Arceos(test_qemu::ArgsArceos),
    /// Run StarryOS QEMU test suite
    Starry(test_qemu::ArgsStarry),
    /// Run Axvisor QEMU test suite
    Axvisor(test_qemu::ArgsAxvisor),
}

#[derive(Subcommand)]
enum UbootTestCommand {
    /// Run Axvisor U-Boot board test suite
    Axvisor(test_qemu::ArgsAxvisorUboot),
}

pub async fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();
    run_root_cli(cli).await
}

async fn run_root_cli(cli: Cli) -> anyhow::Result<()> {
    match cli.command {
        Commands::Test {
            command: TestCommand::Std,
        } => {
            test_std::run_std_test_command()?;
        }
        Commands::Test {
            command:
                TestCommand::Qemu {
                    command: QemuTestCommand::Arceos(args),
                },
        } => {
            test_qemu::run_arceos_qemu_tests(args).await?;
        }
        Commands::Test {
            command:
                TestCommand::Qemu {
                    command: QemuTestCommand::Starry(args),
                },
        } => {
            test_qemu::run_starry_qemu_tests(args).await?;
        }
        Commands::Test {
            command:
                TestCommand::Qemu {
                    command: QemuTestCommand::Axvisor(args),
                },
        } => {
            test_qemu::run_axvisor_qemu_tests(args).await?;
        }
        Commands::Test {
            command:
                TestCommand::Uboot {
                    command: UbootTestCommand::Axvisor(args),
                },
        } => {
            test_qemu::run_axvisor_uboot_tests(args).await?;
        }
        Commands::Axvisor { command } => {
            Axvisor::new()?.execute(command).await?;
        }
        Commands::Arceos { command } => {
            ArceOS::new()?.execute(command).await?;
        }
        Commands::Starry { command } => {
            Starry::new()?.execute(command).await?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn cli_parses_test_std_command() {
        let cli = Cli::try_parse_from(["axbuild", "test", "std"]).unwrap();

        match cli.command {
            Commands::Test {
                command: TestCommand::Std,
            } => {}
            _ => panic!("expected `test std` command"),
        }
    }

    #[test]
    fn cli_parses_test_qemu_arceos_command() {
        let cli = Cli::try_parse_from([
            "axbuild",
            "test",
            "qemu",
            "arceos",
            "--target",
            "x86_64-unknown-none",
        ])
        .unwrap();

        match cli.command {
            Commands::Test {
                command:
                    TestCommand::Qemu {
                        command: QemuTestCommand::Arceos(args),
                    },
            } => assert_eq!(args.target, "x86_64-unknown-none"),
            _ => panic!("expected `test qemu arceos` command"),
        }
    }

    #[test]
    fn cli_parses_test_qemu_starry_command() {
        let cli = Cli::try_parse_from(["axbuild", "test", "qemu", "starry", "--target", "x86_64"])
            .unwrap();

        match cli.command {
            Commands::Test {
                command:
                    TestCommand::Qemu {
                        command: QemuTestCommand::Starry(args),
                    },
            } => assert_eq!(args.target, "x86_64"),
            _ => panic!("expected `test qemu starry` command"),
        }
    }

    #[test]
    fn cli_parses_test_qemu_axvisor_command() {
        let cli =
            Cli::try_parse_from(["axbuild", "test", "qemu", "axvisor", "--target", "aarch64"])
                .unwrap();

        match cli.command {
            Commands::Test {
                command:
                    TestCommand::Qemu {
                        command: QemuTestCommand::Axvisor(args),
                    },
            } => assert_eq!(args.target, "aarch64"),
            _ => panic!("expected `test qemu axvisor` command"),
        }
    }

    #[test]
    fn cli_parses_test_qemu_axvisor_arch_alias() {
        let cli = Cli::try_parse_from(["axbuild", "test", "qemu", "axvisor", "--arch", "aarch64"])
            .unwrap();

        match cli.command {
            Commands::Test {
                command:
                    TestCommand::Qemu {
                        command: QemuTestCommand::Axvisor(args),
                    },
            } => assert_eq!(args.target, "aarch64"),
            _ => panic!("expected `test qemu axvisor` command"),
        }
    }

    #[test]
    fn cli_parses_test_uboot_axvisor_command() {
        let cli = Cli::try_parse_from([
            "axbuild",
            "test",
            "uboot",
            "axvisor",
            "--board",
            "phytiumpi",
        ])
        .unwrap();

        match cli.command {
            Commands::Test {
                command:
                    TestCommand::Uboot {
                        command: UbootTestCommand::Axvisor(args),
                    },
            } => {
                assert_eq!(args.board, "phytiumpi");
                assert_eq!(args.uboot_config, None);
            }
            _ => panic!("expected `test uboot axvisor` command"),
        }
    }

    #[test]
    fn cli_parses_test_uboot_axvisor_short_board_flag() {
        let cli =
            Cli::try_parse_from(["axbuild", "test", "uboot", "axvisor", "-b", "roc-rk3568-pc"])
                .unwrap();

        match cli.command {
            Commands::Test {
                command:
                    TestCommand::Uboot {
                        command: UbootTestCommand::Axvisor(args),
                    },
            } => {
                assert_eq!(args.board, "roc-rk3568-pc");
                assert_eq!(args.uboot_config, None);
            }
            _ => panic!("expected `test uboot axvisor` command"),
        }
    }

    #[test]
    fn cli_parses_test_uboot_axvisor_custom_config() {
        let cli = Cli::try_parse_from([
            "axbuild",
            "test",
            "uboot",
            "axvisor",
            "--board",
            "phytiumpi",
            "--uboot-config",
            ".github/workflows/uboot.toml",
        ])
        .unwrap();

        match cli.command {
            Commands::Test {
                command:
                    TestCommand::Uboot {
                        command: UbootTestCommand::Axvisor(args),
                    },
            } => {
                assert_eq!(args.board, "phytiumpi");
                assert_eq!(
                    args.uboot_config,
                    Some(PathBuf::from(".github/workflows/uboot.toml"))
                );
            }
            _ => panic!("expected `test uboot axvisor` command"),
        }
    }

    #[test]
    fn cli_parses_axvisor_image_ls_command() {
        let cli = Cli::try_parse_from(["axbuild", "axvisor", "image", "ls"]).unwrap();

        match cli.command {
            Commands::Axvisor {
                command: axvisor::Command::Image(_),
            } => {}
            _ => panic!("expected `axvisor image ls` command"),
        }
    }

    #[test]
    fn cli_parses_axvisor_image_pull_command() {
        let cli = Cli::try_parse_from(["axbuild", "axvisor", "image", "pull", "linux"]).unwrap();

        match cli.command {
            Commands::Axvisor {
                command: axvisor::Command::Image(_),
            } => {}
            _ => panic!("expected `axvisor image pull` command"),
        }
    }

    #[test]
    fn cli_parses_axvisor_build_command() {
        let cli = Cli::try_parse_from([
            "axbuild",
            "axvisor",
            "build",
            "--arch",
            "aarch64",
            "--config",
            "os/axvisor/.build.toml",
            "--vmconfigs",
            "tmp/vm1.toml",
        ])
        .unwrap();

        match cli.command {
            Commands::Axvisor {
                command: axvisor::Command::Build(_),
            } => {}
            _ => panic!("expected `axvisor build` command"),
        }
    }

    #[test]
    fn cli_parses_axvisor_qemu_command() {
        let cli = Cli::try_parse_from([
            "axbuild",
            "axvisor",
            "qemu",
            "--arch",
            "aarch64",
            "--config",
            "os/axvisor/.build.toml",
            "--qemu-config",
            "configs/qemu.toml",
            "--vmconfigs",
            "tmp/vm1.toml",
        ])
        .unwrap();

        match cli.command {
            Commands::Axvisor {
                command: axvisor::Command::Qemu(_),
            } => {}
            _ => panic!("expected `axvisor qemu` command"),
        }
    }
}
