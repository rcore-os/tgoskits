#![cfg_attr(not(any(windows, unix)), no_std)]
#![cfg(any(windows, unix))]

use clap::{Args, Parser, Subcommand};

use crate::{arceos::ArceOS, axvisor::Axvisor, starry::Starry};

pub mod arceos;
pub mod axvisor;
mod backtrace;
mod board;
mod build;
mod clippy;
mod config;
pub mod context;
pub mod image;
mod rootfs;
pub mod starry;
mod support;
mod sync_lint;
mod test;

#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Args, Clone, Debug, PartialEq, Eq)]
pub(crate) struct ClippyArgs {
    /// Audit every workspace package
    #[arg(long)]
    pub(crate) all: bool,
    /// Run clippy only for the named workspace package(s)
    #[arg(long = "package", value_name = "PACKAGE")]
    pub(crate) packages: Vec<String>,
    /// Run clippy for workspace packages affected since the git ref
    #[arg(long, value_name = "REF")]
    pub(crate) since: Option<String>,
}

#[derive(Args, Clone, Debug, PartialEq, Eq)]
pub(crate) struct SyncLintArgs {
    /// Run sync-lint only for Rust files changed since the git ref
    #[arg(long, value_name = "REF")]
    pub(crate) since: Option<String>,
}

#[derive(Subcommand)]
enum Commands {
    /// Run std tests for the configured workspace package whitelist
    Test,
    /// Run clippy for workspace packages
    Clippy(ClippyArgs),
    /// Run high-confidence atomic ordering checks for suspicious `Relaxed` synchronization
    SyncLint(SyncLintArgs),
    /// Remote board management via ostool-server
    Board {
        #[command(subcommand)]
        command: board::Command,
    },
    /// Config generation and inspection helpers
    Config {
        #[command(subcommand)]
        command: config::Command,
    },
    /// TGOS image management
    Image(image::ImageArgs),
    /// Backtrace host-side helpers
    Backtrace {
        #[command(subcommand)]
        command: backtrace::Command,
    },
    /// Axvisor host-side commands
    Axvisor {
        #[command(subcommand)]
        command: Box<axvisor::Command>,
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
        Commands::Test => test::std::run_std_test_command(),
        Commands::Clippy(args) => clippy::run_workspace_clippy_command(&args),
        Commands::SyncLint(args) => sync_lint::run_sync_lint_command(&args),
        Commands::Board { command } => board::execute(command).await,
        Commands::Config { command } => config::execute(command),
        Commands::Image(args) => image::run(args).await,
        Commands::Backtrace { command } => backtrace::execute(command),
        Commands::Axvisor { command } => Axvisor::new()?.execute(*command).await,
        Commands::Arceos { command } => ArceOS::new()?.execute(command).await,
        Commands::Starry { command } => Starry::new()?.execute(command).await,
    }
}
