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

#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
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
    match cli.command {
        Commands::Arceos { command } => {
            ArceOS::new()?.execute(command).await?;
        }
        Commands::Starry { command } => {
            Starry::new()?.execute(command).await?;
        }
    }

    Ok(())
}
