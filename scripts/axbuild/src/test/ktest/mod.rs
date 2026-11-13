mod build_config;
mod qemu;
use qemu::*;
mod board;
use board::*;
mod discovery;
use discovery::*;
mod cargo;
use cargo::*;
mod coverage;
use coverage::*;
mod contract;
use contract::*;
mod runtime;
use std::{
    collections::{BTreeMap, HashMap},
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::Command as ProcessCommand,
};

use anyhow::{Context, anyhow, bail};
use build_config::{KtestBuildConfig, KtestBuildSelection};
use cargo_metadata::{Metadata, Package};
use clap::{Args, Subcommand, ValueEnum};
use ostool::{
    board::{RunBoardOptions, config::BoardRunConfig},
    build::config::{Cargo, CargoBuildProfile},
    ovmf::Arch,
    run::{ShellCheckStep, qemu::QemuConfig},
};
use runtime::*;
use serde::Deserialize;

use self::plan::{
    DiscoveredKtestPackage, KtestExecutionUnit, KtestRuntime, PlanFailure, QemuPlanSelector,
    build_qemu_plan,
};
use crate::{arceos, context::AppContext, starry};

mod plan;
#[cfg(test)]
mod tests;

pub(crate) const AXTEST_RUSTFLAGS: &[&str] = &["--cfg", "axtest", "--check-cfg", "cfg(axtest)"];
const AXTEST_FEATURE: &str = "axtest";
const AXTEST_SUITE_OK: &str = "AXTEST_SUITE_OK";
const AXTEST_SUITE_FAIL: &str = "AXTEST_SUITE_FAIL";
const AXTEST_CASE_FAIL: &str = r"AXTEST_CASE\b[^\r\n]*\bstatus=fail\b";
const PANIC_FAIL: &str = "panicked at";
/// A suite result with at least one passing case and no failures.
///
/// `axtest` prints `AXTEST_SUMMARY pass=<n> fail=<n> skip=<n> total=<n>` after
/// the last case, so a suite that selected nothing must not look successful.
const AXTEST_SUMMARY_GATE: &str = r"AXTEST_SUMMARY pass=[1-9][0-9]* fail=0(?-u:\b)";
const COVERAGE_IGNORED_SOURCE_REGEX: &str = r"[/\\](\.(cargo|rustup)|target)[/\\]";

#[derive(Args, Debug, Clone)]
pub(crate) struct ArgsKtest {
    #[command(subcommand)]
    pub(crate) command: Command,
}

#[derive(Subcommand, Debug, Clone)]
pub(crate) enum Command {
    /// Run a kernel axtest target in QEMU
    Qemu(ArgsKtestQemu),
    /// Run a kernel axtest target on a remote board
    Board(ArgsKtestBoard),
}

#[derive(Args, Debug, Clone, Default)]
pub(crate) struct ArgsKtestQemu {
    /// Test every workspace package with a direct axtest dependency
    #[arg(long, conflicts_with = "packages")]
    pub(crate) workspace: bool,

    /// Cargo package(s) that own test targets
    #[arg(short = 'p', long = "package", value_name = "PACKAGE")]
    pub(crate) packages: Vec<String>,

    /// Exclude package(s) from workspace selection
    #[arg(long = "exclude", value_name = "PACKAGE")]
    pub(crate) excludes: Vec<String>,

    /// Cargo test target name(s); otherwise run every harness=false test target
    #[arg(long = "test", value_name = "TARGET")]
    pub(crate) tests: Vec<String>,

    /// Target architecture
    #[arg(long, value_name = "ARCH", conflicts_with = "target")]
    pub(crate) arch: Option<String>,

    /// Rust target triple
    #[arg(short = 't', long, value_name = "TRIPLE", conflicts_with = "arch")]
    pub(crate) target: Option<String>,

    /// Features to enable, separated by commas or repeated
    #[arg(long, value_delimiter = ',', value_name = "FEATURES")]
    pub(crate) features: Vec<String>,

    /// Enable every feature of the selected package
    #[arg(long = "all-features", conflicts_with = "no_default_features")]
    pub(crate) all_features: bool,

    /// Do not enable the selected package's default feature
    #[arg(long = "no-default-features")]
    pub(crate) no_default_features: bool,

    /// Cargo build profile
    #[arg(long, value_name = "PROFILE")]
    pub(crate) profile: Option<String>,

    /// Cargo target directory
    #[arg(long = "target-dir", value_name = "DIRECTORY")]
    pub(crate) target_dir: Option<PathBuf>,

    /// Require Cargo.lock to remain unchanged
    #[arg(long)]
    pub(crate) locked: bool,

    /// Run without accessing the network
    #[arg(long)]
    pub(crate) offline: bool,

    /// Equivalent to --locked and --offline
    #[arg(long)]
    pub(crate) frozen: bool,

    /// Build TOML path
    #[arg(long = "config", value_name = "BUILD_TOML")]
    pub(crate) config: Option<PathBuf>,

    /// Built-in compilation preset, optionally combined with a sparse TOML
    #[arg(long, value_name = "NAME")]
    pub(crate) preset: Option<String>,

    /// QEMU TOML path
    #[arg(long = "qemu-config", value_name = "QEMU_TOML")]
    pub(crate) qemu_config: Option<PathBuf>,

    /// Enable axtest coverage capture
    #[arg(long)]
    pub(crate) coverage: bool,

    /// Generate coverage report in the selected format
    #[arg(
        long = "out-fmt",
        value_enum,
        value_name = "FMT",
        requires = "coverage"
    )]
    pub(crate) out_fmt: Option<KtestCoverageOutFmt>,

    /// Continue running execution units after a failure
    #[arg(long = "no-fail-fast")]
    pub(crate) no_fail_fast: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum KtestCoverageOutFmt {
    Html,
}

#[derive(Args, Debug, Clone)]
pub(crate) struct ArgsKtestBoard {
    /// Cargo package that owns the test target
    #[arg(short = 'p', long = "package", value_name = "PACKAGE")]
    pub(crate) package: String,

    /// Cargo test target name
    #[arg(long = "test", value_name = "TARGET")]
    pub(crate) test: String,

    /// Board/default config name
    #[arg(short = 'b', long = "board", value_name = "BOARD")]
    pub(crate) board: String,

    /// Build TOML path
    #[arg(long = "config", value_name = "BUILD_TOML")]
    pub(crate) config: Option<PathBuf>,

    /// Built-in compilation preset, independent of the remote board endpoint
    #[arg(long, value_name = "NAME")]
    pub(crate) preset: Option<String>,

    /// Board TOML path
    #[arg(long = "board-config", value_name = "BOARD_TOML")]
    pub(crate) board_config: Option<PathBuf>,

    /// Override ostool board type
    #[arg(long = "board-type", value_name = "TYPE")]
    pub(crate) board_type: Option<String>,

    /// ostool-server host
    #[arg(long)]
    pub(crate) server: Option<String>,

    /// ostool-server port
    #[arg(long)]
    pub(crate) port: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct KtestPackage {
    pub(crate) name: String,
    pub(crate) targets: Vec<KtestTarget>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct KtestTarget {
    pub(crate) name: String,
    pub(crate) kind: KtestTargetKind,
    pub(crate) harness: bool,
    pub(crate) required_features: Vec<String>,
    /// Declared through `[package.metadata.axtest.targets.<name>] board_only`.
    ///
    /// Board-only targets depend on hardware that no QEMU run can provide, so
    /// the QEMU plan skips them unless the caller names them explicitly, and an
    /// explicit QEMU selection then reports the board runner instead.
    pub(crate) board_only: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KtestTargetKind {
    Lib,
    Test,
    Other,
}

#[derive(Debug, Clone, Copy)]
struct KtestBuildContext<'a> {
    runtime: KtestRuntime,
    workspace_root: &'a Path,
    package: &'a str,
    arch: &'a str,
    target: &'a str,
    config: &'a KtestBuildConfig,
}

/// The build and run configuration owned by one axtest execution unit.
#[derive(Debug, Clone, Copy)]
struct KtestUnitConfig<'a> {
    package: &'a str,
    arch: &'a str,
    target: &'a str,
    run_config: &'a Path,
    config: &'a KtestBuildConfig,
}

pub(crate) async fn run(args: ArgsKtest) -> anyhow::Result<()> {
    match args.command {
        Command::Qemu(args) => run_qemu(args).await,
        Command::Board(args) => run_board(args).await,
    }
}

impl ArgsKtestQemu {
    fn qemu_selector(&self) -> QemuPlanSelector {
        QemuPlanSelector {
            workspace: self.workspace,
            packages: self.packages.clone(),
            excludes: self.excludes.clone(),
            tests: self.tests.clone(),
            arch: self.arch.clone(),
            target: self.target.clone(),
        }
    }
}
