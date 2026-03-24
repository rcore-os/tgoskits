#![cfg_attr(not(any(windows, unix)), no_std)]
#![cfg(any(windows, unix))]

use clap::{Parser, Subcommand};

use crate::arceos::ArceOS;

pub mod arceos;
pub mod context;
mod logging;

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
}

pub async fn run() -> anyhow::Result<()> {

    let cli = Cli::parse();

    match cli.command {
        Commands::Arceos { command } => {
            ArceOS::new().execute(command).await?;
        }
    }

    Ok(())
}
