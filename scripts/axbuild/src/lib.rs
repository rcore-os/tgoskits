#![cfg_attr(not(any(windows, unix)), no_std)]
#![cfg(any(windows, unix))]

#[macro_use]
extern crate log;

#[macro_use]
extern crate anyhow;

use clap::{Args, Parser, Subcommand};

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
mod rootfs;
pub mod starry;
mod sync_lint;
mod test_qemu;
mod test_std;

#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Args, Clone, Debug, PartialEq, Eq)]
pub(crate) struct ClippyArgs {
    /// Audit every workspace package instead of the maintained whitelist
    #[arg(long)]
    pub(crate) all: bool,
    /// Run clippy only for the named workspace package(s)
    #[arg(long = "package", value_name = "PACKAGE")]
    pub(crate) packages: Vec<String>,
}

#[derive(Subcommand)]
enum Commands {
    /// Run std tests for the configured workspace package whitelist
    Test,
    /// Run clippy for the maintained whitelist by default
    Clippy(ClippyArgs),
    /// Run high-confidence atomic ordering checks for suspicious `Relaxed` synchronization
    SyncLint,
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
        Commands::Clippy(args) => clippy::run_workspace_clippy_command(&args),
        Commands::SyncLint => sync_lint::run_sync_lint_command(),
        Commands::Board { command } => board::execute(command).await,
        Commands::Axvisor { command } => Axvisor::new()?.execute(command).await,
        Commands::Arceos { command } => ArceOS::new()?.execute(command).await,
        Commands::Starry { command } => Starry::new()?.execute(command).await,
    }
}
