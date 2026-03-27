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
    /// Run std tests for the configured workspace package whitelist
    Test,
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

pub async fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();
    run_root_cli(cli).await
}

async fn run_root_cli(cli: Cli) -> anyhow::Result<()> {
    match cli.command {
        Commands::Test => {
            test_std::run_std_test_command()?;
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
    fn cli_parses_test_command() {
        let cli = Cli::try_parse_from(["axbuild", "test"]).unwrap();

        match cli.command {
            Commands::Test => {}
            _ => panic!("expected `test` command"),
        }
    }

    #[test]
    fn cli_rejects_legacy_test_std_command() {
        assert!(Cli::try_parse_from(["axbuild", "test", "std"]).is_err());
    }

    #[test]
    fn cli_rejects_legacy_test_qemu_command() {
        assert!(Cli::try_parse_from(["axbuild", "test", "qemu", "arceos"]).is_err());
    }

    #[test]
    fn cli_rejects_legacy_test_uboot_command() {
        assert!(Cli::try_parse_from(["axbuild", "test", "uboot", "axvisor"]).is_err());
    }

    #[test]
    fn cli_parses_arceos_test_qemu_command() {
        let cli = Cli::try_parse_from([
            "axbuild",
            "arceos",
            "test",
            "qemu",
            "--target",
            "x86_64-unknown-none",
        ])
        .unwrap();

        match cli.command {
            Commands::Arceos {
                command: arceos::Command::Test(args),
            } => match args.command {
                arceos::TestCommand::Qemu(args) => assert_eq!(args.target, "x86_64-unknown-none"),
                _ => panic!("expected `arceos test qemu` command"),
            },
            _ => panic!("expected `arceos test qemu` command"),
        }
    }

    #[test]
    fn cli_parses_starry_test_qemu_command() {
        let cli = Cli::try_parse_from(["axbuild", "starry", "test", "qemu", "--target", "x86_64"])
            .unwrap();

        match cli.command {
            Commands::Starry {
                command: starry::Command::Test(args),
            } => match args.command {
                starry::TestCommand::Qemu(args) => assert_eq!(args.target, "x86_64"),
                _ => panic!("expected `starry test qemu` command"),
            },
            _ => panic!("expected `starry test qemu` command"),
        }
    }

    #[test]
    fn cli_parses_axvisor_test_qemu_command() {
        let cli =
            Cli::try_parse_from(["axbuild", "axvisor", "test", "qemu", "--target", "aarch64"])
                .unwrap();

        match cli.command {
            Commands::Axvisor {
                command: axvisor::Command::Test(args),
            } => match args.command {
                axvisor::TestCommand::Qemu(args) => assert_eq!(args.target, "aarch64"),
                _ => panic!("expected `axvisor test qemu` command"),
            },
            _ => panic!("expected `axvisor test qemu` command"),
        }
    }

    #[test]
    fn cli_parses_axvisor_test_qemu_arch_alias() {
        let cli = Cli::try_parse_from(["axbuild", "axvisor", "test", "qemu", "--arch", "aarch64"])
            .unwrap();

        match cli.command {
            Commands::Axvisor {
                command: axvisor::Command::Test(args),
            } => match args.command {
                axvisor::TestCommand::Qemu(args) => assert_eq!(args.target, "aarch64"),
                _ => panic!("expected `axvisor test qemu` command"),
            },
            _ => panic!("expected `axvisor test qemu` command"),
        }
    }

    #[test]
    fn cli_parses_arceos_test_uboot_command() {
        let cli = Cli::try_parse_from(["axbuild", "arceos", "test", "uboot"]).unwrap();

        match cli.command {
            Commands::Arceos {
                command: arceos::Command::Test(args),
            } => match args.command {
                arceos::TestCommand::Uboot(_) => {}
                _ => panic!("expected `arceos test uboot` command"),
            },
            _ => panic!("expected `arceos test uboot` command"),
        }
    }

    #[test]
    fn cli_parses_starry_test_uboot_command() {
        let cli = Cli::try_parse_from(["axbuild", "starry", "test", "uboot"]).unwrap();

        match cli.command {
            Commands::Starry {
                command: starry::Command::Test(args),
            } => match args.command {
                starry::TestCommand::Uboot(_) => {}
                _ => panic!("expected `starry test uboot` command"),
            },
            _ => panic!("expected `starry test uboot` command"),
        }
    }

    #[test]
    fn cli_parses_axvisor_test_uboot_command() {
        let cli = Cli::try_parse_from([
            "axbuild",
            "axvisor",
            "test",
            "uboot",
            "--board",
            "phytiumpi",
        ])
        .unwrap();

        match cli.command {
            Commands::Axvisor {
                command: axvisor::Command::Test(args),
            } => match args.command {
                axvisor::TestCommand::Uboot(args) => {
                    assert_eq!(args.board, "phytiumpi");
                    assert_eq!(args.uboot_config, None);
                }
                _ => panic!("expected `axvisor test uboot` command"),
            },
            _ => panic!("expected `axvisor test uboot` command"),
        }
    }

    #[test]
    fn cli_parses_axvisor_test_uboot_short_board_flag() {
        let cli =
            Cli::try_parse_from(["axbuild", "axvisor", "test", "uboot", "-b", "roc-rk3568-pc"])
                .unwrap();

        match cli.command {
            Commands::Axvisor {
                command: axvisor::Command::Test(args),
            } => match args.command {
                axvisor::TestCommand::Uboot(args) => {
                    assert_eq!(args.board, "roc-rk3568-pc");
                    assert_eq!(args.uboot_config, None);
                }
                _ => panic!("expected `axvisor test uboot` command"),
            },
            _ => panic!("expected `axvisor test uboot` command"),
        }
    }

    #[test]
    fn cli_parses_axvisor_test_uboot_custom_config() {
        let cli = Cli::try_parse_from([
            "axbuild",
            "axvisor",
            "test",
            "uboot",
            "--board",
            "phytiumpi",
            "--uboot-config",
            ".github/workflows/uboot.toml",
        ])
        .unwrap();

        match cli.command {
            Commands::Axvisor {
                command: axvisor::Command::Test(args),
            } => match args.command {
                axvisor::TestCommand::Uboot(args) => {
                    assert_eq!(args.board, "phytiumpi");
                    assert_eq!(
                        args.uboot_config,
                        Some(PathBuf::from(".github/workflows/uboot.toml"))
                    );
                }
                _ => panic!("expected `axvisor test uboot` command"),
            },
            _ => panic!("expected `axvisor test uboot` command"),
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
