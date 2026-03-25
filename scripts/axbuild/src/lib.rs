#![cfg_attr(not(any(windows, unix)), no_std)]
#![cfg(any(windows, unix))]

#[macro_use]
extern crate log;

#[macro_use]
extern crate anyhow;

use clap::{Parser, Subcommand};

use crate::{arceos::ArceOS, starry::Starry};

pub mod arceos;
pub mod context;
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
}

#[derive(Subcommand)]
enum QemuTestCommand {
    /// Run ArceOS QEMU test suites
    Arceos(test_qemu::ArgsArceos),
    /// Run StarryOS QEMU test suite
    Starry(test_qemu::ArgsStarry),
}

pub async fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();
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
}
