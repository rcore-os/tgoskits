#![cfg_attr(not(any(windows, unix)), no_std)]
#![cfg(any(windows, unix))]

use clap::{Args, Parser, Subcommand};

use crate::{arceos::ArceOS, axloader::Axloader, axvisor::Axvisor, starry::Starry};

pub mod arceos;
pub mod axloader;
pub mod axvisor;
mod backtrace;
mod board;
mod build;
mod clippy;
mod config;
pub mod context;
mod firmware;
pub mod image;
mod ktest;
mod rootfs;
mod spin_lint;
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
    /// Run kernel axtest targets through QEMU or a remote board
    Ktest(ktest::ArgsKtest),
    /// Run clippy for workspace packages
    Clippy(ClippyArgs),
    /// Run high-confidence atomic ordering checks for suspicious `Relaxed` synchronization
    SyncLint(SyncLintArgs),
    /// Verify that no external `spin` package is resolved
    SpinLint,
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
    /// Backtrace host-side helpers
    Backtrace {
        #[command(subcommand)]
        command: backtrace::Command,
    },
    /// TGOS image management
    Image(image::ImageArgs),
    /// Axvisor host-side commands
    Axvisor {
        #[command(subcommand)]
        command: axvisor::Command,
    },
    /// Axloader host-side commands
    Axloader {
        #[command(subcommand)]
        command: axloader::Command,
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
        Commands::Ktest(args) => ktest::run(args).await,
        Commands::Clippy(args) => {
            ensure_aic8800_firmware().await?;
            clippy::run_workspace_clippy_command(&args)
        }
        Commands::SyncLint(args) => sync_lint::run_sync_lint_command(&args),
        Commands::SpinLint => spin_lint::run_spin_lint_command(),
        Commands::Board { command } => board::execute(command).await,
        Commands::Config { command } => config::execute(command),
        Commands::Backtrace { command } => backtrace::execute(command),
        Commands::Image(args) => image::run(args).await,
        Commands::Axvisor { command } => Axvisor::new()?.execute(command).await,
        Commands::Axloader { command } => Axloader::new()?.execute(command).await,
        Commands::Arceos { command } => ArceOS::new()?.execute(command).await,
        Commands::Starry { command } => {
            ensure_aic8800_firmware().await?;
            Starry::new()?.execute(command).await
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use clap::Parser;

    use super::*;

    #[derive(Parser)]
    struct TestCli {
        #[command(subcommand)]
        command: Commands,
    }

    #[test]
    fn command_parses_ktest_qemu() {
        let cli = TestCli::try_parse_from([
            "xtask",
            "ktest",
            "qemu",
            "-p",
            "starry-kernel",
            "--test",
            "axtest_kernel",
            "--arch",
            "x86_64",
            "--qemu-config",
            "qemu.toml",
            "--coverage",
        ])
        .unwrap();

        match cli.command {
            Commands::Ktest(args) => match args.command {
                ktest::Command::Qemu(args) => {
                    assert_eq!(args.package, "starry-kernel");
                    assert_eq!(args.test.as_deref(), Some("axtest_kernel"));
                    assert_eq!(args.arch.as_deref(), Some("x86_64"));
                    assert_eq!(args.qemu_config, Some(PathBuf::from("qemu.toml")));
                    assert!(args.coverage);
                }
                _ => panic!("expected ktest qemu command"),
            },
            _ => panic!("expected ktest command"),
        }
    }

    #[test]
    fn command_parses_ktest_board() {
        let cli = TestCli::try_parse_from([
            "xtask",
            "ktest",
            "board",
            "-p",
            "starry-kernel",
            "--test",
            "axtest_kernel",
            "-b",
            "orangepi-5-plus",
            "--board-config",
            "board.toml",
            "--board-type",
            "OrangePi-5-Plus",
            "--server",
            "10.0.0.2",
            "--port",
            "9000",
        ])
        .unwrap();

        match cli.command {
            Commands::Ktest(args) => match args.command {
                ktest::Command::Board(args) => {
                    assert_eq!(args.package, "starry-kernel");
                    assert_eq!(args.test, "axtest_kernel");
                    assert_eq!(args.board, "orangepi-5-plus");
                    assert_eq!(args.board_config, Some(PathBuf::from("board.toml")));
                    assert_eq!(args.board_type.as_deref(), Some("OrangePi-5-Plus"));
                    assert_eq!(args.server.as_deref(), Some("10.0.0.2"));
                    assert_eq!(args.port, Some(9000));
                }
                _ => panic!("expected ktest board command"),
            },
            _ => panic!("expected ktest command"),
        }
    }
}

/// Provisions the AIC8800 Wi-Fi firmware blobs (fetched + integrity-checked,
/// never committed) before any command that may compile the `aic8800` crate.
async fn ensure_aic8800_firmware() -> anyhow::Result<()> {
    let workspace_root = context::workspace_root_path()?;
    firmware::ensure_aic8800_firmware(&workspace_root).await
}
