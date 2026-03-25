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
}

pub async fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Test {
            command: TestCommand::Std,
        } => {
            test_std::run_std_test_command()?;
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
}
