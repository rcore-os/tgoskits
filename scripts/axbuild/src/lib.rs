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
mod board;
mod clippy;
mod command_flow;
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
    /// Run clippy for each root workspace package and each named feature in isolation
    Clippy,
    /// Remote board management via ostool-server
    Board {
        #[command(subcommand)]
        command: board::Command,
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

pub async fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();
    run_root_cli(cli).await
}

async fn run_root_cli(cli: Cli) -> anyhow::Result<()> {
    match cli.command {
        Commands::Test => test_std::run_std_test_command(),
        Commands::Clippy => clippy::run_workspace_clippy_command(),
        Commands::Board { command } => board::execute(command).await,
        Commands::Axvisor { command } => Axvisor::new()?.execute(command).await,
        Commands::Arceos { command } => ArceOS::new()?.execute(command).await,
        Commands::Starry { command } => Starry::new()?.execute(command).await,
    }
}

#[cfg(test)]
mod tests {
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
    fn cli_parses_clippy_command() {
        let cli = Cli::try_parse_from(["axbuild", "clippy"]).unwrap();

        match cli.command {
            Commands::Clippy => {}
            _ => panic!("expected `clippy` command"),
        }
    }

    #[test]
    fn cli_parses_board_ls_command() {
        let cli = Cli::try_parse_from([
            "axbuild", "board", "ls", "--server", "10.0.0.2", "--port", "9000",
        ])
        .unwrap();

        match cli.command {
            Commands::Board {
                command: board::Command::Ls(server),
            } => {
                assert_eq!(server.server.as_deref(), Some("10.0.0.2"));
                assert_eq!(server.port, Some(9000));
            }
            _ => panic!("expected `board ls` command"),
        }
    }

    #[test]
    fn cli_parses_board_connect_command() {
        let cli = Cli::try_parse_from([
            "axbuild",
            "board",
            "connect",
            "-b",
            "rk3568",
            "--server",
            "board.example",
        ])
        .unwrap();

        match cli.command {
            Commands::Board {
                command: board::Command::Connect(args),
            } => {
                assert_eq!(args.board_type, "rk3568");
                assert_eq!(args.server.server.as_deref(), Some("board.example"));
                assert_eq!(args.server.port, None);
            }
            _ => panic!("expected `board connect` command"),
        }
    }

    #[test]
    fn cli_parses_board_config_command() {
        let cli = Cli::try_parse_from(["axbuild", "board", "config"]).unwrap();

        match cli.command {
            Commands::Board {
                command: board::Command::Config,
            } => {}
            _ => panic!("expected `board config` command"),
        }
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
    fn cli_parses_arceos_branch_command() {
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
            Commands::Arceos { .. } => {}
            _ => panic!("expected `arceos` branch command"),
        }
    }

    #[test]
    fn cli_parses_starry_branch_command() {
        let cli = Cli::try_parse_from(["axbuild", "starry", "test", "qemu", "--target", "x86_64"])
            .unwrap();

        match cli.command {
            Commands::Starry { .. } => {}
            _ => panic!("expected `starry` branch command"),
        }
    }

    #[test]
    fn cli_parses_axvisor_branch_command() {
        let cli = Cli::try_parse_from(["axbuild", "axvisor", "image", "ls"]).unwrap();

        match cli.command {
            Commands::Axvisor { .. } => {}
            _ => panic!("expected `axvisor` branch command"),
        }
    }
}
