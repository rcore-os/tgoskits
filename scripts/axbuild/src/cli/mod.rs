use clap::{Args, Parser, Subcommand};

use crate::{
    arceos,
    arceos::ArceOS,
    axloader,
    axloader::Axloader,
    axvisor,
    axvisor::Axvisor,
    checks::{clippy, sync_lint},
    image, starry,
    starry::Starry,
    support,
    test::{self, ktest},
    tools::{agent_review_bench, axbt, board},
};

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
    /// Number of independent Cargo workers (default: serial execution)
    #[arg(long, default_value_t = 1)]
    pub(crate) jobs: usize,
    /// Write a portable check plan instead of running Clippy.
    #[arg(long, value_name = "FILE")]
    pub(crate) emit_plan: Option<std::path::PathBuf>,
    /// Number of independent CI shards in an emitted plan.
    #[arg(long, default_value_t = 4)]
    pub(crate) shards: usize,
    /// Execute only the selected shard of a previously emitted plan.
    #[arg(long, value_name = "FILE", conflicts_with_all = ["emit_plan", "verify_results", "all", "packages"])]
    pub(crate) execute_plan: Option<std::path::PathBuf>,
    /// One-based shard number to execute.
    #[arg(long, requires = "execute_plan")]
    pub(crate) shard: Option<usize>,
    /// Publish the selected shard's check results, including failures.
    #[arg(long, requires = "execute_plan")]
    pub(crate) result_file: Option<std::path::PathBuf>,
    /// Verify that every check in a plan has a successful shard result.
    #[arg(long, value_name = "FILE", conflicts_with_all = ["emit_plan", "execute_plan", "all", "packages"])]
    pub(crate) verify_results: Option<std::path::PathBuf>,
    #[arg(long, requires = "verify_results")]
    pub(crate) results_dir: Option<std::path::PathBuf>,
}

#[derive(Args, Clone, Debug, PartialEq, Eq)]
pub(crate) struct SyncLintArgs {
    /// Run sync-lint only for Rust files changed since the git ref
    #[arg(long, value_name = "REF")]
    pub(crate) since: Option<String>,
}

#[derive(Subcommand)]
enum Commands {
    /// Run offline Codex review benchmarks from historical PR snapshots
    AgentReviewBench {
        #[command(subcommand)]
        command: agent_review_bench::Command,
    },
    /// Generate a target-side AXBT map from a final ELF
    Axbt(axbt::AxbtArgs),
    /// Run std tests for the configured workspace package whitelist
    Test(test::std::StdTestArgs),
    /// Run statically linked workspace crate tests through qemu-user
    CrossTest(test::cross::CrossTestArgs),
    /// Run kernel axtest targets through QEMU or a remote board
    Ktest(ktest::ArgsKtest),
    /// Run clippy for workspace packages
    Clippy(ClippyArgs),
    /// Run high-confidence atomic ordering checks for suspicious `Relaxed` synchronization
    SyncLint(SyncLintArgs),
    /// Remote board management via ostool-server
    Board {
        #[command(subcommand)]
        command: board::Command,
    },
    /// TGOS image management
    Image(image::ImageArgs),
    /// Fetch verified OVMF firmware and print its paths as JSON
    Ovmf(support::ovmf::OvmfArgs),
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

/// Like [`run`], but parses from an explicit argument list instead of
/// [`std::env::args_os`].  Used by external tools (e.g. the axvisor
/// xtask) that dispatch a sub‑command through axbuild's own CLI.
pub async fn run_from<I, T>(args: I) -> anyhow::Result<()>
where
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString> + Clone,
{
    let cli = Cli::parse_from(args);
    run_root_cli(cli).await
}

async fn run_root_cli(cli: Cli) -> anyhow::Result<()> {
    match cli.command {
        Commands::AgentReviewBench { command } => agent_review_bench::execute(command).await,
        Commands::Axbt(args) => axbt::run(args),
        Commands::Test(args) => test::std::run_std_test_command(&args),
        Commands::CrossTest(args) => test::cross::run(args),
        Commands::Ktest(args) => ktest::run(args).await,
        Commands::Clippy(args) => clippy::run_workspace_clippy_command(&args),
        Commands::SyncLint(args) => sync_lint::run_sync_lint_command(&args),
        Commands::Board { command } => board::execute(command).await,
        Commands::Image(args) => image::run(args).await,
        Commands::Ovmf(args) => support::ovmf::execute(args).await,
        Commands::Axvisor { command } => Axvisor::new()?.execute(command).await,
        Commands::Axloader { command } => Axloader::new()?.execute(command).await,
        Commands::Arceos { command } => ArceOS::new()?.execute(command).await,
        Commands::Starry { command } => Starry::new()?.execute(command).await,
    }
}
