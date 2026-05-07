use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use anyhow::{Context, bail};
use clap::{Args, Subcommand};
use ostool::{board::RunBoardOptions, build::config::Cargo, run::qemu::QemuConfig};
use serde::Deserialize;

use super::{Starry, board, build, rootfs};
use crate::{
    context::{
        ResolvedStarryRequest, SnapshotPersistence, StarryCliArgs, arch_for_target_checked,
        resolve_starry_arch_and_target, validate_supported_target,
    },
    test::{
        board as board_test, case,
        case::{TestQemuCase, TestQemuSubcase, TestQemuSubcaseKind},
        qemu as qemu_test, suite as test_suite,
    },
};

const STARRY_TEST_SUITE_OS: &str = "starryos";

#[derive(Args)]
pub struct ArgsTest {
    #[command(subcommand)]
    pub command: TestCommand,
}

#[derive(Subcommand)]
pub enum TestCommand {
    /// Run StarryOS QEMU test suite
    Qemu(ArgsTestQemu),
    /// Run StarryOS remote board test suite
    Board(ArgsTestBoard),
}

#[derive(Args, Debug, Clone)]
pub struct ArgsTestQemu {
    #[arg(
        long,
        value_name = "ARCH",
        required_unless_present_any = ["target", "list"],
        help = "StarryOS architecture to test"
    )]
    pub arch: Option<String>,
    #[arg(
        short = 't',
        long,
        value_name = "TARGET",
        required_unless_present_any = ["arch", "list"],
        help = "StarryOS target triple to test"
    )]
    pub target: Option<String>,
    #[arg(
        short = 'g',
        long = "test-group",
        value_name = "GROUP",
        help = "Run StarryOS QEMU test cases from one test group"
    )]
    pub test_group: Option<String>,
    #[arg(
        short = 'c',
        long = "test-case",
        value_name = "CASE",
        help = "Run only one StarryOS QEMU test case"
    )]
    pub test_case: Option<String>,
    #[arg(long, help = "Run stress StarryOS qemu test cases")]
    pub stress: bool,
    #[arg(short = 'l', long, help = "List discovered StarryOS QEMU test cases")]
    pub list: bool,
}

#[derive(Args, Debug, Clone, Default)]
pub struct ArgsTestBoard {
    #[arg(
        short = 'g',
        long = "test-group",
        value_name = "GROUP",
        help = "Run Starry board test cases from one test group"
    )]
    pub test_group: Option<String>,

    #[arg(
        short = 'c',
        long = "test-case",
        value_name = "CASE",
        help = "Run only one Starry board test case"
    )]
    pub test_case: Option<String>,

    #[arg(
        long,
        value_name = "BOARD",
        help = "Run all Starry board test cases for one board"
    )]
    pub board: Option<String>,

    #[arg(short = 'b', long = "board-type", value_name = "BOARD_TYPE")]
    pub board_type: Option<String>,

    #[arg(long)]
    pub server: Option<String>,

    #[arg(long)]
    pub port: Option<u16>,

    #[arg(short = 'l', long, help = "List discovered Starry board test cases")]
    pub list: bool,
}

pub(super) async fn test(starry: &mut Starry, args: ArgsTest) -> anyhow::Result<()> {
    match args.command {
        TestCommand::Qemu(args) => starry.test_qemu(args).await,
        TestCommand::Board(args) => starry.test_board(args).await,
    }
}

const STARRY_NORMAL_GROUP: &str = "normal";
const STARRY_STRESS_GROUP: &str = "stress";

pub(crate) fn resolve_qemu_test_group_name(
    selected_group: Option<&str>,
    stress: bool,
) -> anyhow::Result<String> {
    if stress {
        if let Some(group) = selected_group
            && group != STARRY_STRESS_GROUP
        {
            bail!(
                "`--stress` is equivalent to `--test-group stress` and cannot be combined with \
                 `--test-group {group}`"
            );
        }
        return Ok(STARRY_STRESS_GROUP.to_string());
    }

    Ok(selected_group.unwrap_or(STARRY_NORMAL_GROUP).to_string())
}

/// Starry-specific extra fields in a QEMU test case TOML.
///
/// Only the fields that are not part of `ostool`'s `QemuConfig` are read here.
/// The remaining fields (including `shell_init_cmd`) are read as `QemuConfig`
/// later in `prepare_qemu_cases` so we avoid parsing the same file twice.
#[derive(Debug, Deserialize)]
struct StarryQemuCaseConfig {
    #[serde(default)]
    test_commands: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StarryQemuCaseOutcome {
    Passed,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StarryQemuCaseReport {
    pub(crate) name: String,
    pub(crate) outcome: StarryQemuCaseOutcome,
    pub(crate) duration: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StarryQemuRunReport {
    pub(crate) group: String,
    pub(crate) cases: Vec<StarryQemuCaseReport>,
    pub(crate) total_duration: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StarryBoardTestGroup {
    pub(crate) name: String,
    pub(crate) board_name: String,
    pub(crate) arch: String,
    pub(crate) target: String,
    pub(crate) build_config_path: PathBuf,
    pub(crate) board_test_config_path: PathBuf,
}

impl board_test::BoardTestGroupInfo for StarryBoardTestGroup {
    fn name(&self) -> &str {
        &self.name
    }

    fn board_name(&self) -> &str {
        &self.board_name
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StarryQemuCaseRequirements {
    smp: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StarryQemuCase {
    case: TestQemuCase,
    build_group: String,
    build_config_path: PathBuf,
}

#[derive(Debug, Clone)]
struct PreparedStarryQemuCase {
    case: TestQemuCase,
    qemu: QemuConfig,
    build_group: String,
    build_config_path: PathBuf,
    rootfs_path: PathBuf,
    requirements: StarryQemuCaseRequirements,
}

impl qemu_test::BuildConfigRef for PreparedStarryQemuCase {
    fn build_group(&self) -> &str {
        &self.build_group
    }

    fn build_config_path(&self) -> &Path {
        &self.build_config_path
    }
}

pub(crate) fn parse_test_target(
    workspace_root: &Path,
    arch: &Option<String>,
    target: &Option<String>,
) -> anyhow::Result<(String, String)> {
    let supported_targets = board::board_default_list(workspace_root)?
        .into_iter()
        .filter(|board| board.name.starts_with("qemu-"))
        .map(|board| board.target)
        .collect::<Vec<_>>();

    let supported_target_refs = supported_targets
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    let supported_arches = supported_targets
        .iter()
        .map(|target| arch_for_target_checked(target))
        .collect::<anyhow::Result<BTreeSet<_>>>()?
        .into_iter()
        .collect::<Vec<_>>();

    let (arch, target) = resolve_starry_arch_and_target(arch.clone(), target.clone())?;
    validate_supported_target(&arch, "starry qemu tests", "arch values", &supported_arches)?;
    validate_supported_target(
        &target,
        "starry qemu tests",
        "targets",
        &supported_target_refs,
    )?;
    Ok((arch, target))
}

pub(crate) fn discover_qemu_cases(
    workspace_root: &Path,
    arch: &str,
    target: &str,
    selected_case: Option<&str>,
    group: &str,
) -> anyhow::Result<Vec<StarryQemuCase>> {
    let test_suite_dir = require_test_suite_group_dir(workspace_root, group)?;
    qemu_test::discover_qemu_cases(
        &test_suite_dir,
        arch,
        target,
        selected_case,
        "Starry",
        group,
    )?
    .into_iter()
    .map(load_qemu_case)
    .collect()
}

fn load_qemu_case(case: qemu_test::DiscoveredQemuCase) -> anyhow::Result<StarryQemuCase> {
    let test_commands = load_qemu_case_test_commands(&case.qemu_config_path)?;
    let subcases = if test_commands.is_empty() {
        Vec::new()
    } else {
        discover_qemu_subcases(&case.case_dir)?
    };

    let test_case = TestQemuCase {
        display_name: case.display_name,
        name: case.name,
        case_dir: case.case_dir,
        qemu_config_path: case.qemu_config_path,
        test_commands,
        subcases,
    };

    Ok(StarryQemuCase {
        case: test_case,
        build_group: case.build_group,
        build_config_path: case.build_config_path,
    })
}

/// Parses `test_commands` from a Starry QEMU case TOML.
///
/// Only the Starry-specific `test_commands` field is read here.  The
/// mutual-exclusion check against `shell_init_cmd` is deferred to
/// `prepare_qemu_cases` where the full `QemuConfig` (including
/// `shell_init_cmd`) is already available, avoiding a second file read.
fn load_qemu_case_test_commands(qemu_config_path: &Path) -> anyhow::Result<Vec<String>> {
    let content = fs::read_to_string(qemu_config_path)
        .with_context(|| format!("failed to read {}", qemu_config_path.display()))?;
    let config: StarryQemuCaseConfig = toml::from_str(&content)
        .with_context(|| format!("failed to parse {}", qemu_config_path.display()))?;

    qemu_test::normalize_qemu_test_commands(qemu_config_path, config.test_commands, "Starry")
}

fn discover_qemu_subcases(case_dir: &Path) -> anyhow::Result<Vec<TestQemuSubcase>> {
    let mut subcases = Vec::new();
    for entry in
        fs::read_dir(case_dir).with_context(|| format!("failed to read {}", case_dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        let kind = if path.join("c").is_dir() {
            Some(TestQemuSubcaseKind::C)
        } else if path.join("rust").is_dir() {
            Some(TestQemuSubcaseKind::Rust)
        } else {
            None
        };

        if let Some(kind) = kind {
            subcases.push(TestQemuSubcase {
                name,
                case_dir: path,
                kind,
            });
        }
    }
    subcases.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(subcases)
}

pub(crate) fn finalize_qemu_case_run(report: &StarryQemuRunReport) -> anyhow::Result<()> {
    println!("{}", render_qemu_case_summary(report));

    let failed = report
        .cases
        .iter()
        .filter(|case| case.outcome == StarryQemuCaseOutcome::Failed)
        .map(|case| case.name.clone())
        .collect::<Vec<_>>();

    if failed.is_empty() {
        Ok(())
    } else {
        bail!(
            "starry {} qemu tests failed for {} case(s): {}",
            report.group.as_str(),
            failed.len(),
            failed.join(", ")
        )
    }
}

pub(crate) fn discover_board_test_groups(
    workspace_root: &Path,
    group: &str,
    selected_case: Option<&str>,
    selected_board: Option<&str>,
) -> anyhow::Result<Vec<StarryBoardTestGroup>> {
    let test_suite_dir = require_test_suite_group_dir(workspace_root, group)?;
    let groups = collect_board_test_groups(workspace_root, &test_suite_dir)?;
    board_test::filter_board_test_groups(groups, selected_case, selected_board, "Starry", || {
        format!(
            "no Starry board test groups found under {}",
            test_suite_dir.display()
        )
    })
}

fn require_test_suite_group_dir(workspace_root: &Path, group: &str) -> anyhow::Result<PathBuf> {
    test_suite::require_group_dir(workspace_root, STARRY_TEST_SUITE_OS, "Starry", group)
}

fn test_suite_root(workspace_root: &Path) -> PathBuf {
    test_suite::suite_root(workspace_root, STARRY_TEST_SUITE_OS)
}

fn discover_test_group_names(workspace_root: &Path) -> anyhow::Result<Vec<String>> {
    test_suite::discover_group_names(workspace_root, STARRY_TEST_SUITE_OS)
}

fn discover_all_qemu_cases_in_group(
    workspace_root: &Path,
    group: &str,
    selected_case: Option<&str>,
) -> anyhow::Result<Vec<String>> {
    let test_suite_dir = require_test_suite_group_dir(workspace_root, group)?;
    qemu_test::discover_all_qemu_cases(&test_suite_dir, selected_case, "Starry", group)
}

fn discover_all_qemu_cases_with_archs_in_group(
    workspace_root: &Path,
    group: &str,
    selected_case: Option<&str>,
) -> anyhow::Result<Vec<qemu_test::ListedQemuCase>> {
    let test_suite_dir = require_test_suite_group_dir(workspace_root, group)?;
    qemu_test::discover_all_qemu_cases_with_archs(&test_suite_dir, selected_case, "Starry", group)
}

fn render_qemu_case_summary(report: &StarryQemuRunReport) -> String {
    let passed = report
        .cases
        .iter()
        .filter(|case| case.outcome == StarryQemuCaseOutcome::Passed)
        .collect::<Vec<_>>();
    let failed = report
        .cases
        .iter()
        .filter(|case| case.outcome == StarryQemuCaseOutcome::Failed)
        .collect::<Vec<_>>();

    let mut lines = Vec::new();
    lines.push(format!("starry {} qemu summary:", report.group));
    lines.push(format!("passed ({}):", passed.len()));
    if passed.is_empty() {
        lines.push("  <none>".to_string());
    } else {
        lines.extend(
            passed
                .iter()
                .map(|case| format!("  {} ({})", case.name, format_duration(case.duration))),
        );
    }

    lines.push(format!("failed ({}):", failed.len()));
    if failed.is_empty() {
        lines.push("  <none>".to_string());
    } else {
        lines.extend(
            failed
                .iter()
                .map(|case| format!("  {} ({})", case.name, format_duration(case.duration))),
        );
    }

    lines.push(format!("total: {}", format_duration(report.total_duration)));
    lines.join("\n")
}

fn format_duration(duration: Duration) -> String {
    format!("{:.2}s", duration.as_secs_f64())
}

fn collect_board_test_groups(
    _workspace_root: &Path,
    test_suite_dir: &Path,
) -> anyhow::Result<Vec<StarryBoardTestGroup>> {
    let mut groups = Vec::new();
    for config in board_test::discover_board_runtime_configs(test_suite_dir)? {
        let wrapper =
            qemu_test::nearest_build_wrapper(test_suite_dir, &config.case_dir, "Starry", "board")?;
        let board_file = board::load_board_file(&wrapper.build_config_path).with_context(|| {
            format!(
                "failed to load Starry board build config `{}`",
                wrapper.build_config_path.display()
            )
        })?;
        let arch = arch_for_target_checked(&board_file.target)?.to_string();
        let target = board_file.target;
        groups.push(StarryBoardTestGroup {
            name: qemu_test::case_name_from_wrapper(test_suite_dir, &wrapper, &config.case_dir)?,
            board_name: config.board_name,
            arch,
            target,
            build_config_path: wrapper.build_config_path,
            board_test_config_path: config.config_path,
        });
    }

    Ok(groups)
}

impl Starry {
    pub(super) async fn test_qemu(&mut self, args: ArgsTestQemu) -> anyhow::Result<()> {
        if args.list
            && args.arch.is_none()
            && args.target.is_none()
            && args.test_group.is_none()
            && !args.stress
        {
            let groups = discover_test_group_names(self.app.workspace_root())?
                .into_iter()
                .filter_map(|group| {
                    match discover_all_qemu_cases_with_archs_in_group(
                        self.app.workspace_root(),
                        &group,
                        args.test_case.as_deref(),
                    ) {
                        Ok(case_names) => Some(Ok((group, case_names))),
                        Err(err) => {
                            let message = err.to_string();
                            if message.starts_with("no Starry ")
                                || message.starts_with("unknown Starry ")
                            {
                                None
                            } else {
                                Some(Err(err))
                            }
                        }
                    }
                })
                .collect::<anyhow::Result<Vec<_>>>()?;
            if groups.is_empty() {
                bail!(
                    "no Starry qemu test cases found under {}",
                    test_suite_root(self.app.workspace_root()).display()
                );
            }
            println!("{}", qemu_test::render_qemu_case_forest("starry", groups));
            return Ok(());
        }

        if args.list && args.arch.is_none() && args.target.is_none() {
            if args.stress
                && let Some(group) = args.test_group.as_deref()
                && group != STARRY_STRESS_GROUP
            {
                bail!(
                    "`--stress` is equivalent to `--test-group stress` and cannot be combined \
                     with `--test-group {group}`"
                );
            }
            let group = args.test_group.as_deref().unwrap_or(if args.stress {
                STARRY_STRESS_GROUP
            } else {
                STARRY_NORMAL_GROUP
            });
            let case_names = discover_all_qemu_cases_in_group(
                self.app.workspace_root(),
                group,
                args.test_case.as_deref(),
            )?;
            if case_names.is_empty()
                && let Some(case) = args.test_case.as_deref()
            {
                bail!("unknown Starry {group} qemu test case `{case}`");
            }
            println!("{}", qemu_test::render_case_tree(group, case_names));
            return Ok(());
        }

        let test_group = resolve_qemu_test_group_name(args.test_group.as_deref(), args.stress)?;
        let (arch, target) =
            parse_test_target(self.app.workspace_root(), &args.arch, &args.target)?;
        let cases = discover_qemu_cases(
            self.app.workspace_root(),
            &arch,
            &target,
            args.test_case.as_deref(),
            &test_group,
        )?;
        if args.list {
            let case_names = cases.iter().map(|case| case.case.name.as_str());
            println!("{}", qemu_test::render_case_tree(&test_group, case_names));
            return Ok(());
        }
        let package = crate::context::STARRY_PACKAGE;

        println!(
            "running starry {} qemu tests for package {} on arch: {} (target: {})",
            test_group, package, arch, target
        );

        let default_board = board::default_board_for_target(self.app.workspace_root(), &target)?;
        let request = self.prepare_request(
            Self::test_build_args(&target, None),
            None,
            None,
            SnapshotPersistence::Discard,
        )?;
        let mut request = Self::qemu_test_request(request);
        if let Some(default_board) = default_board {
            request.plat_dyn = Some(default_board.build_info.plat_dyn);
            request.build_info_override = Some(default_board.build_info);
        } else {
            anyhow::bail!(
                "missing Starry qemu defconfig for target `{target}` in tests; expected a default \
                 qemu board config under os/StarryOS/configs/board"
            );
        }
        let default_rootfs_path =
            crate::rootfs::store::default_rootfs_path(self.app.workspace_root(), &request.arch)?;
        let cases = self
            .prepare_qemu_cases(&request, &default_rootfs_path, cases)
            .await
            .context("failed to load Starry qemu test cases")?;
        self.app.set_debug_mode(request.debug)?;

        let total = cases.len();
        let suite_started = Instant::now();
        let mut reports = Vec::new();
        let case_groups = Self::group_qemu_cases_by_build_config(&cases);
        let mut completed = 0;
        for group in case_groups {
            let (group_request, group_cargo) =
                Self::qemu_group_build_context(&request, group.build_config_path)?;
            self.app
                .build(group_cargo.clone(), group_request.build_info_path.clone())
                .await
                .with_context(|| {
                    format!(
                        "failed to build Starry qemu test artifact for build group `{}` ({})",
                        group.build_group,
                        group.build_config_path.display()
                    )
                })?;

            for case in group.cases {
                completed += 1;
                let case_name = &case.case.name;
                println!("[{completed}/{total}] starry qemu {case_name}");

                let case_started = Instant::now();
                match self
                    .run_qemu_case(&group_request, &group_cargo, case)
                    .await
                    .with_context(|| format!("starry qemu test failed for case `{case_name}`"))
                {
                    Ok(()) => {
                        println!("ok: {case_name}");
                        reports.push(StarryQemuCaseReport {
                            name: case_name.clone(),
                            outcome: StarryQemuCaseOutcome::Passed,
                            duration: case_started.elapsed(),
                        });
                    }
                    Err(err) => {
                        eprintln!("failed: {case_name}: {err:#}");
                        reports.push(StarryQemuCaseReport {
                            name: case_name.clone(),
                            outcome: StarryQemuCaseOutcome::Failed,
                            duration: case_started.elapsed(),
                        });
                    }
                }
            }
        }

        finalize_qemu_case_run(&StarryQemuRunReport {
            group: test_group,
            cases: reports,
            total_duration: suite_started.elapsed(),
        })
    }

    pub(super) async fn test_board(&mut self, args: ArgsTestBoard) -> anyhow::Result<()> {
        if args.list && args.test_group.is_none() {
            let groups = discover_test_group_names(self.app.workspace_root())?
                .into_iter()
                .filter_map(|group| {
                    match discover_board_test_groups(
                        self.app.workspace_root(),
                        &group,
                        args.test_case.as_deref(),
                        args.board.as_deref(),
                    ) {
                        Ok(groups) => Some(Ok((
                            group,
                            groups
                                .into_iter()
                                .map(|group| (group.name, group.board_name))
                                .collect::<Vec<_>>(),
                        ))),
                        Err(err) => {
                            let message = err.to_string();
                            if message.starts_with("no Starry ") {
                                None
                            } else {
                                Some(Err(err))
                            }
                        }
                    }
                })
                .collect::<anyhow::Result<Vec<_>>>()?;
            if groups.is_empty() {
                bail!(
                    "no Starry board test groups found under {}",
                    test_suite_root(self.app.workspace_root()).display()
                );
            }
            println!(
                "{}",
                qemu_test::render_labeled_case_forest("starry", groups)
            );
            return Ok(());
        }

        let test_group = args.test_group.as_deref().unwrap_or(STARRY_NORMAL_GROUP);
        let groups = discover_board_test_groups(
            self.app.workspace_root(),
            test_group,
            args.test_case.as_deref(),
            args.board.as_deref(),
        )?;
        if args.list {
            let case_names = groups
                .into_iter()
                .map(|group| (group.name, group.board_name))
                .collect::<Vec<_>>();
            println!(
                "{}",
                qemu_test::render_labeled_case_forest("starry", [(test_group, case_names)])
            );
            return Ok(());
        }
        let total = groups.len();
        let mut failed = Vec::new();

        for (index, group) in groups.into_iter().enumerate() {
            let group_label = format!("{}/{}", group.name, group.board_name);
            let board_test_config = group.board_test_config_path.clone();
            let board_test_config_summary = board_test_config.display().to_string();

            if !board_test_config.exists() {
                eprintln!(
                    "failed: {}: missing board test config `{}`",
                    group_label, board_test_config_summary
                );
                failed.push(group_label);
                continue;
            }

            println!("[{}/{}] starry board {}", index + 1, total, group_label);

            let result = async {
                let request = self.prepare_request(
                    Self::test_board_build_args(&group),
                    None,
                    None,
                    SnapshotPersistence::Discard,
                )?;
                let cargo = build::load_cargo_config(&request)?;
                let board_config = self
                    .load_board_config(&cargo, Some(board_test_config.as_path()))
                    .await?;
                self.app
                    .board(
                        cargo,
                        request.build_info_path,
                        board_config,
                        RunBoardOptions {
                            board_type: args.board_type.clone(),
                            server: args.server.clone(),
                            port: args.port,
                        },
                    )
                    .await
                    .with_context(|| {
                        format!(
                            "starry board test failed for group `{}` (build_config={}, \
                             board_test_config={})",
                            group_label,
                            group.build_config_path.display(),
                            board_test_config_summary
                        )
                    })
            }
            .await;

            match result {
                Ok(()) => println!("ok: {}", group_label),
                Err(err) => {
                    eprintln!("failed: {}: {:#}", group_label, err);
                    failed.push(group_label);
                }
            }
        }

        board_test::finalize_board_test_run("starry", &failed)
    }

    fn test_build_args(target: &str, config: Option<PathBuf>) -> StarryCliArgs {
        StarryCliArgs {
            config,
            arch: None,
            target: Some(target.to_string()),
            smp: None,
            debug: false,
        }
    }

    fn test_board_build_args(group: &StarryBoardTestGroup) -> StarryCliArgs {
        StarryCliArgs {
            config: Some(group.build_config_path.clone()),
            arch: None,
            target: Some(group.target.clone()),
            smp: None,
            debug: false,
        }
    }

    async fn prepare_qemu_cases(
        &mut self,
        request: &ResolvedStarryRequest,
        default_rootfs_path: &Path,
        cases: Vec<StarryQemuCase>,
    ) -> anyhow::Result<Vec<PreparedStarryQemuCase>> {
        let mut prepared = Vec::with_capacity(cases.len());
        for starry_case in cases {
            let mut request = request.clone();
            request.build_info_path = starry_case.build_config_path.clone();
            let cargo = build::load_cargo_config(&request)?;
            let qemu = self
                .app
                .tool_mut()
                .read_qemu_config_from_path_for_cargo(&cargo, &starry_case.case.qemu_config_path)
                .await
                .with_context(|| {
                    format!(
                        "failed to read Starry qemu config for case `{}`",
                        starry_case.case.display_name
                    )
                })?;
            let rootfs_path =
                Self::qemu_case_rootfs_path(self.app.workspace_root(), &qemu, default_rootfs_path);
            if rootfs_path == default_rootfs_path {
                rootfs::ensure_rootfs_in_target_dir(
                    self.app.workspace_root(),
                    &request.arch,
                    &request.target,
                )
                .await?;
            } else {
                crate::rootfs::store::ensure_optional_managed_rootfs(
                    self.app.workspace_root(),
                    &request.arch,
                    Some(&rootfs_path),
                )
                .await?;
            }
            qemu_test::validate_grouped_qemu_commands(&qemu, &starry_case.case, "Starry")?;
            let requirements = Self::qemu_case_requirements(&qemu).with_context(|| {
                format!(
                    "failed to read QEMU requirements for `{}`",
                    starry_case.case.display_name
                )
            })?;
            prepared.push(PreparedStarryQemuCase {
                case: starry_case.case,
                qemu,
                build_group: starry_case.build_group,
                build_config_path: starry_case.build_config_path,
                rootfs_path,
                requirements,
            });
        }

        Ok(prepared)
    }

    fn qemu_case_rootfs_path(
        workspace_root: &Path,
        qemu: &QemuConfig,
        default_rootfs_path: &Path,
    ) -> PathBuf {
        let managed_rootfs_dir = crate::rootfs::store::rootfs_dir(workspace_root);
        qemu.args
            .windows(2)
            .find_map(|args| {
                (args[0] == "-drive")
                    .then(|| args[1].strip_prefix("id=disk0,if=none,format=raw,file="))
                    .flatten()
                    .map(PathBuf::from)
                    .filter(|path| path.starts_with(&managed_rootfs_dir))
            })
            .unwrap_or_else(|| default_rootfs_path.to_path_buf())
    }

    fn qemu_case_requirements(qemu: &QemuConfig) -> anyhow::Result<StarryQemuCaseRequirements> {
        Ok(StarryQemuCaseRequirements {
            smp: qemu_test::smp_from_qemu_arg(qemu).unwrap_or(1),
        })
    }

    fn group_qemu_cases_by_build_config(
        cases: &[PreparedStarryQemuCase],
    ) -> Vec<qemu_test::QemuCaseGroup<'_, PreparedStarryQemuCase>> {
        qemu_test::group_cases_by_build_config(cases)
    }

    fn qemu_group_build_context(
        request: &ResolvedStarryRequest,
        build_config_path: &Path,
    ) -> anyhow::Result<(ResolvedStarryRequest, Cargo)> {
        let mut request = request.clone();
        request.build_info_path = build_config_path.to_path_buf();
        request.build_info_override = None;
        let cargo = build::load_cargo_config(&request)?;

        Ok((request, cargo))
    }

    fn qemu_test_request(mut request: ResolvedStarryRequest) -> ResolvedStarryRequest {
        request.smp = None;
        request
    }

    async fn run_qemu_case(
        &mut self,
        request: &ResolvedStarryRequest,
        cargo: &Cargo,
        prepared_case: &PreparedStarryQemuCase,
    ) -> anyhow::Result<()> {
        let case = &prepared_case.case;
        let mut qemu = prepared_case.qemu.clone();
        let asset_config = starry_case_asset_config();
        case::apply_grouped_qemu_config(&mut qemu, case, &asset_config.grouped_runner);

        qemu_test::apply_smp_qemu_arg(&mut qemu, Some(prepared_case.requirements.smp));
        qemu_test::apply_timeout_scale(&mut qemu);

        let prepare_started = Instant::now();
        let prepared_assets = case::prepare_case_assets(
            self.app.workspace_root(),
            &request.arch,
            &request.target,
            case,
            prepared_case.rootfs_path.clone(),
            asset_config,
        )
        .await?;
        println!(
            "  prepare assets: {:.2?} (pipeline={}, cache={})",
            prepare_started.elapsed(),
            prepared_assets.pipeline.as_str(),
            if prepared_assets.cache_hit {
                "hit"
            } else {
                "miss"
            }
        );
        rootfs::patch_rootfs(
            &mut qemu,
            &prepared_assets.rootfs_path,
            rootfs::RootfsPatchMode::EnsureDiskBootNet,
        );
        qemu.args.extend(prepared_assets.extra_qemu_args.clone());

        println!(
            "  qemu config: {} (timeout={})",
            case.qemu_config_path.display(),
            qemu_test::qemu_timeout_summary(&qemu)
        );
        println!("  rootfs: {}", prepared_assets.rootfs_path.display());
        let qemu_started = Instant::now();
        let result = self.app.run_qemu(cargo, qemu).await;
        println!("  qemu run: {:.2?}", qemu_started.elapsed());
        // Remove the per-case rootfs copy immediately after the run so disk
        // usage stays bounded to ~1 active copy at a time rather than
        // accumulating one copy per case.
        case::remove_case_rootfs_copy(prepared_assets.rootfs_copy_to_remove.as_deref());
        case::remove_case_run_dir(prepared_assets.run_dir_to_remove.as_deref());
        result
    }
}

pub(crate) fn starry_case_asset_config() -> case::CaseAssetConfig {
    case::CaseAssetConfig {
        grouped_runner: case::GroupedCaseRunnerConfig {
            runner_name: "starry-run-case-tests".to_string(),
            runner_path: "/usr/bin/starry-run-case-tests".to_string(),
            begin_marker: "STARRY_GROUPED_TEST_BEGIN".to_string(),
            passed_marker: "STARRY_GROUPED_TEST_PASSED".to_string(),
            failed_marker: "STARRY_GROUPED_TEST_FAILED".to_string(),
            all_passed_marker: "STARRY_GROUPED_TESTS_PASSED".to_string(),
            all_failed_marker: "STARRY_GROUPED_TESTS_FAILED".to_string(),
            success_regex: r"(?m)^STARRY_GROUPED_TESTS_PASSED\s*$".to_string(),
            fail_regex: r"(?m)^STARRY_GROUPED_TEST_FAILED:".to_string(),
        },
        script_env: case::CaseScriptEnvConfig {
            staging_root: "STARRY_STAGING_ROOT".to_string(),
            case_dir: "STARRY_CASE_DIR".to_string(),
            case_c_dir: "STARRY_CASE_C_DIR".to_string(),
            case_work_dir: "STARRY_CASE_WORK_DIR".to_string(),
            case_build_dir: "STARRY_CASE_BUILD_DIR".to_string(),
            case_overlay_dir: "STARRY_CASE_OVERLAY_DIR".to_string(),
        },
        cache_env_vars: vec![crate::starry::apk::STARRY_APK_REGION_VAR.to_string()],
        prepare_staging_root: crate::starry::resolver::write_host_resolver_config,
        prepare_guest_package_env: Some(starry_guest_package_env),
    }
}

fn starry_guest_package_env(staging_root: &Path) -> anyhow::Result<Vec<(String, String)>> {
    let region = crate::starry::apk::apk_region_from_env()?;
    crate::starry::apk::rewrite_apk_repositories_for_region(staging_root, region)?;
    log_starry_apk_prebuild_context(staging_root, region)?;
    Ok(vec![(
        crate::starry::apk::STARRY_APK_REGION_VAR.to_string(),
        region.canonical_name().to_string(),
    )])
}

fn log_starry_apk_prebuild_context(
    staging_root: &Path,
    region: crate::starry::apk::ApkRegion,
) -> anyhow::Result<()> {
    let repositories_path = staging_root.join("etc/apk/repositories");
    let repositories = fs::read_to_string(&repositories_path)
        .with_context(|| format!("failed to read {}", repositories_path.display()))?;

    println!("STARRY_APK_REGION={}", region.canonical_name());
    println!("apk repositories:");
    print!("{repositories}");
    if !repositories.ends_with('\n') {
        println!();
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{fs, time::Duration};

    use tempfile::tempdir;

    use super::*;

    fn write_qemu_build_config(
        root: &Path,
        group: &str,
        build_group: &str,
        target: &str,
    ) -> PathBuf {
        let path = root
            .join("test-suit/starryos")
            .join(group)
            .join(build_group)
            .join(format!("build-{target}.toml"));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            format!("target = \"{target}\"\nfeatures = [\"qemu\"]\nlog = \"Info\"\n"),
        )
        .unwrap();
        path
    }

    fn write_qemu_build_config_with_max_cpu_num(
        root: &Path,
        group: &str,
        build_group: &str,
        target: &str,
        max_cpu_num: usize,
    ) -> PathBuf {
        let path = root
            .join("test-suit/starryos")
            .join(group)
            .join(build_group)
            .join(format!("build-{target}.toml"));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            format!(
                "target = \"{target}\"\nenv = {{}}\nfeatures = [\"qemu\"]\nlog = \
                 \"Info\"\nmax_cpu_num = {max_cpu_num}\n"
            ),
        )
        .unwrap();
        path
    }

    fn write_starry_board_build_config(root: &Path, build_group: &str, target: &str) -> PathBuf {
        let path = root
            .join("test-suit/starryos/normal")
            .join(build_group)
            .join(format!("build-{target}.toml"));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            format!("target = \"{target}\"\nenv = {{}}\nfeatures = [\"qemu\"]\nlog = \"Info\"\n"),
        )
        .unwrap();
        path
    }

    fn starry_request(path: PathBuf, arch: &str, target: &str) -> ResolvedStarryRequest {
        ResolvedStarryRequest {
            package: crate::context::STARRY_PACKAGE.to_string(),
            arch: arch.to_string(),
            target: target.to_string(),
            plat_dyn: None,
            smp: None,
            debug: false,
            build_info_path: path,
            build_info_override: None,
            qemu_config: None,
            uboot_config: None,
        }
    }

    fn write_board_test_config(
        root: &Path,
        build_group: &str,
        case_name: &str,
        board_name: &str,
    ) -> PathBuf {
        let path = root
            .join("test-suit/starryos/normal")
            .join(build_group)
            .join(case_name)
            .join(format!("board-{board_name}.toml"));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            "board_type = \"OrangePi-5-Plus\"\nshell_prefix = \
             \"orangepi@orangepi5plus:~\"\nshell_init_cmd = \"pwd && echo 'test \
             pass'\"\nsuccess_regex = [\"(?m)^test pass\\\\s*$\"]\nfail_regex = []\ntimeout = \
             300\n",
        )
        .unwrap();
        path
    }

    #[test]
    fn discovers_board_test_group_and_build_mapping() {
        let root = tempdir().unwrap();
        let build_config = write_starry_board_build_config(
            root.path(),
            "orangepi-5-plus",
            "aarch64-unknown-none-softfloat",
        );
        let board_test_config =
            write_board_test_config(root.path(), "orangepi-5-plus", "smoke", "orangepi-5-plus");

        let groups = discover_board_test_groups(root.path(), "normal", None, None).unwrap();

        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].name, "smoke");
        assert_eq!(groups[0].board_name, "orangepi-5-plus");
        assert_eq!(groups[0].arch, "aarch64");
        assert_eq!(groups[0].target, "aarch64-unknown-none-softfloat");
        assert_eq!(groups[0].build_config_path, build_config);
        assert_eq!(groups[0].board_test_config_path, board_test_config);
    }

    #[test]
    fn discovers_board_case_when_case_dir_contains_build_config() {
        let root = tempdir().unwrap();
        let case_dir = root.path().join("test-suit/starryos/normal/smoke");
        fs::create_dir_all(&case_dir).unwrap();
        let build_config = case_dir.join("build-aarch64-unknown-none-softfloat.toml");
        fs::write(
            &build_config,
            "target = \"aarch64-unknown-none-softfloat\"\nenv = {}\nfeatures = [\"qemu\"]\nlog = \
             \"Info\"\n",
        )
        .unwrap();
        let board_test_config = case_dir.join("board-orangepi-5-plus.toml");
        fs::write(
            &board_test_config,
            "board_type = \"OrangePi-5-Plus\"\nshell_prefix = \
             \"orangepi@orangepi5plus:~\"\nshell_init_cmd = \"pwd && echo 'test \
             pass'\"\nsuccess_regex = [\"(?m)^test pass\\\\s*$\"]\nfail_regex = []\ntimeout = \
             300\n",
        )
        .unwrap();

        let groups = discover_board_test_groups(root.path(), "normal", None, None).unwrap();

        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].name, "smoke");
        assert_eq!(groups[0].board_name, "orangepi-5-plus");
        assert_eq!(groups[0].build_config_path, build_config);
        assert_eq!(groups[0].board_test_config_path, board_test_config);
    }

    #[test]
    fn filters_board_test_group_by_case() {
        let root = tempdir().unwrap();
        write_starry_board_build_config(
            root.path(),
            "orangepi-5-plus",
            "aarch64-unknown-none-softfloat",
        );
        write_starry_board_build_config(root.path(), "vision-five2", "riscv64gc-unknown-none-elf");
        write_board_test_config(root.path(), "orangepi-5-plus", "smoke", "orangepi-5-plus");
        write_board_test_config(root.path(), "vision-five2", "smoke", "vision-five2");

        let groups =
            discover_board_test_groups(root.path(), "normal", Some("smoke"), None).unwrap();

        assert_eq!(groups.len(), 2);
        assert_eq!(
            groups
                .iter()
                .map(|group| format!("{}/{}", group.name, group.board_name))
                .collect::<Vec<_>>(),
            vec!["smoke/orangepi-5-plus", "smoke/vision-five2"]
        );
    }

    #[test]
    fn filters_board_test_groups_by_board() {
        let root = tempdir().unwrap();
        write_starry_board_build_config(
            root.path(),
            "orangepi-5-plus",
            "aarch64-unknown-none-softfloat",
        );
        write_starry_board_build_config(root.path(), "vision-five2", "riscv64gc-unknown-none-elf");
        write_board_test_config(root.path(), "orangepi-5-plus", "smoke", "orangepi-5-plus");
        write_board_test_config(root.path(), "orangepi-5-plus", "syscall", "orangepi-5-plus");
        write_board_test_config(root.path(), "vision-five2", "smoke", "vision-five2");

        let groups =
            discover_board_test_groups(root.path(), "normal", None, Some("orangepi-5-plus"))
                .unwrap();

        assert_eq!(
            groups
                .iter()
                .map(|group| format!("{}/{}", group.name, group.board_name))
                .collect::<Vec<_>>(),
            vec!["smoke/orangepi-5-plus", "syscall/orangepi-5-plus"]
        );
    }

    #[test]
    fn rejects_unknown_board_test_board() {
        let root = tempdir().unwrap();
        write_starry_board_build_config(
            root.path(),
            "orangepi-5-plus",
            "aarch64-unknown-none-softfloat",
        );
        write_board_test_config(root.path(), "orangepi-5-plus", "smoke", "orangepi-5-plus");

        let err =
            discover_board_test_groups(root.path(), "normal", None, Some("unknown")).unwrap_err();

        assert!(
            err.to_string()
                .contains("unsupported Starry board test board `unknown`")
        );
        assert!(err.to_string().contains("orangepi-5-plus"));
    }

    #[test]
    fn rejects_missing_mapped_board_build_config() {
        let root = tempdir().unwrap();
        write_board_test_config(root.path(), "orangepi-5-plus", "smoke", "orangepi-5-plus");

        let err = discover_board_test_groups(root.path(), "normal", None, None)
            .unwrap_err()
            .to_string();

        assert!(err.contains("not under a build wrapper"));
        assert!(err.contains("smoke"));
    }

    fn write_qemu_test_config(
        root: &Path,
        group: &str,
        build_group: &str,
        case_name: &str,
        arch: &str,
    ) {
        let path = root
            .join("test-suit/starryos")
            .join(group)
            .join(build_group)
            .join(case_name)
            .join(format!("qemu-{arch}.toml"));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "timeout = 1\n").unwrap();
    }

    fn write_grouped_qemu_test_config(
        root: &Path,
        group: &str,
        build_group: &str,
        case_name: &str,
        arch: &str,
    ) {
        let path = root
            .join("test-suit/starryos")
            .join(group)
            .join(build_group)
            .join(case_name)
            .join(format!("qemu-{arch}.toml"));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            path,
            "shell_prefix = \"root@starry:\"\ntest_commands = [\"/usr/bin/beta\", \
             \"/usr/bin/alpha\"]\ntimeout = 1\n",
        )
        .unwrap();
    }

    fn prepared_qemu_case(name: &str, build_config_path: PathBuf) -> PreparedStarryQemuCase {
        PreparedStarryQemuCase {
            case: TestQemuCase {
                name: name.to_string(),
                display_name: name.to_string(),
                case_dir: PathBuf::from(format!("/tmp/{name}")),
                qemu_config_path: PathBuf::from(format!("/tmp/{name}/qemu-x86_64.toml")),
                test_commands: Vec::new(),
                subcases: Vec::new(),
            },
            qemu: QemuConfig::default(),
            build_group: "default".to_string(),
            build_config_path,
            rootfs_path: PathBuf::from("/tmp/rootfs.img"),
            requirements: StarryQemuCaseRequirements { smp: 1 },
        }
    }

    #[test]
    fn discovers_only_cases_with_matching_qemu_config() {
        let root = tempdir().unwrap();
        write_qemu_build_config(root.path(), "normal", "default", "x86_64-unknown-none");
        write_qemu_test_config(root.path(), "normal", "default", "smoke", "x86_64");
        fs::create_dir_all(root.path().join("test-suit/starryos/normal/default/usb")).unwrap();

        let cases =
            discover_qemu_cases(root.path(), "x86_64", "x86_64-unknown-none", None, "normal")
                .unwrap();

        assert_eq!(cases.len(), 1);
        assert_eq!(cases[0].case.name, "smoke");
        assert!(cases[0].case.test_commands.is_empty());
        assert!(cases[0].case.subcases.is_empty());
        assert_eq!(
            cases[0].case.case_dir,
            root.path().join("test-suit/starryos/normal/default/smoke")
        );
    }

    #[test]
    fn discovers_grouped_case_commands_and_sorted_subcases() {
        let root = tempdir().unwrap();
        write_qemu_build_config(root.path(), "normal", "default", "x86_64-unknown-none");
        write_grouped_qemu_test_config(root.path(), "normal", "default", "bugfix", "x86_64");
        fs::create_dir_all(
            root.path()
                .join("test-suit/starryos/normal/default/bugfix/beta/c"),
        )
        .unwrap();
        fs::create_dir_all(
            root.path()
                .join("test-suit/starryos/normal/default/bugfix/alpha/c"),
        )
        .unwrap();

        let cases =
            discover_qemu_cases(root.path(), "x86_64", "x86_64-unknown-none", None, "normal")
                .unwrap();

        assert_eq!(cases.len(), 1);
        assert_eq!(cases[0].case.name, "bugfix");
        assert_eq!(
            cases[0].case.test_commands,
            vec!["/usr/bin/beta".to_string(), "/usr/bin/alpha".to_string()]
        );
        assert_eq!(
            cases[0]
                .case
                .subcases
                .iter()
                .map(|subcase| subcase.name.as_str())
                .collect::<Vec<_>>(),
            vec!["alpha", "beta"]
        );
        assert!(
            cases[0]
                .case
                .subcases
                .iter()
                .all(|subcase| subcase.kind == TestQemuSubcaseKind::C)
        );
    }

    #[test]
    fn grouped_case_loads_with_both_shell_init_cmd_and_test_commands_present() {
        // The mutual-exclusion check has been moved from the initial TOML parse
        // (discover_qemu_cases) to prepare_qemu_cases so we only read each
        // file once.  Therefore, discovery itself should succeed here; the
        // conflict is detected later when QemuConfig is available.
        let root = tempdir().unwrap();
        write_qemu_build_config(root.path(), "normal", "default", "x86_64-unknown-none");
        let path = root
            .path()
            .join("test-suit/starryos/normal/default/bugfix/qemu-x86_64.toml");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            "shell_prefix = \"root@starry:\"\nshell_init_cmd = \"/usr/bin/old\"\ntest_commands = \
             [\"/usr/bin/new\"]\n",
        )
        .unwrap();

        // Discovery no longer validates the shell_init_cmd / test_commands
        // conflict; it should succeed and leave a grouped case behind.
        let cases = discover_qemu_cases(
            root.path(),
            "x86_64",
            "x86_64-unknown-none",
            Some("bugfix"),
            "normal",
        )
        .unwrap();
        assert_eq!(cases.len(), 1);
        assert!(!cases[0].case.test_commands.is_empty());
    }

    #[test]
    fn grouped_case_rejects_empty_test_command() {
        let root = tempdir().unwrap();
        write_qemu_build_config(root.path(), "normal", "default", "x86_64-unknown-none");
        let path = root
            .path()
            .join("test-suit/starryos/normal/default/bugfix/qemu-x86_64.toml");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "test_commands = [\"/usr/bin/ok\", \"  \"]\n").unwrap();

        let err = discover_qemu_cases(
            root.path(),
            "x86_64",
            "x86_64-unknown-none",
            Some("bugfix"),
            "normal",
        )
        .unwrap_err()
        .to_string();

        assert!(err.contains("contains an empty test command"));
    }

    #[test]
    fn selected_case_requires_matching_qemu_config() {
        let root = tempdir().unwrap();
        write_qemu_build_config(root.path(), "normal", "default", "x86_64-unknown-none");
        fs::create_dir_all(root.path().join("test-suit/starryos/normal/default/usb")).unwrap();

        let err = discover_qemu_cases(
            root.path(),
            "x86_64",
            "x86_64-unknown-none",
            Some("usb"),
            "normal",
        )
        .unwrap_err()
        .to_string();

        assert!(err.contains("none provide `qemu-x86_64.toml`"));
        assert!(err.contains("qemu-x86_64.toml"));
    }

    #[test]
    fn selected_qemu_case_skips_non_qemu_case_with_same_name() {
        let root = tempdir().unwrap();
        write_qemu_build_config(
            root.path(),
            "normal",
            "board-orangepi-5-plus",
            "x86_64-unknown-none",
        );
        write_qemu_build_config(root.path(), "normal", "qemu-smp1", "x86_64-unknown-none");
        fs::create_dir_all(
            root.path()
                .join("test-suit/starryos/normal/board-orangepi-5-plus/smoke"),
        )
        .unwrap();
        fs::write(
            root.path().join(
                "test-suit/starryos/normal/board-orangepi-5-plus/smoke/board-orangepi-5-plus.toml",
            ),
            "board_type = \"OrangePi-5-Plus\"\n",
        )
        .unwrap();
        write_qemu_test_config(root.path(), "normal", "qemu-smp1", "smoke", "x86_64");

        let cases = discover_qemu_cases(
            root.path(),
            "x86_64",
            "x86_64-unknown-none",
            Some("smoke"),
            "normal",
        )
        .unwrap();

        assert_eq!(cases.len(), 1);
        assert_eq!(cases[0].build_group, "qemu-smp1");
        assert_eq!(cases[0].case.name, "smoke");
    }

    #[test]
    fn qemu_case_requirements_read_smp_from_case_config() {
        let qemu = QemuConfig {
            args: vec![
                "-nographic".to_string(),
                "-smp".to_string(),
                "cpus=4".to_string(),
            ],
            ..Default::default()
        };

        let requirements = Starry::qemu_case_requirements(&qemu).unwrap();

        assert_eq!(requirements, StarryQemuCaseRequirements { smp: 4 });
    }

    #[test]
    fn qemu_case_requirements_default_to_single_cpu() {
        let qemu = QemuConfig::default();

        let requirements = Starry::qemu_case_requirements(&qemu).unwrap();

        assert_eq!(requirements, StarryQemuCaseRequirements { smp: 1 });
    }

    #[test]
    fn qemu_case_rootfs_uses_drive_file_arg() {
        let root = tempdir().unwrap();
        let managed_rootfs = root.path().join("target/rootfs/rootfs-riscv64-debian.img");
        let qemu = QemuConfig {
            args: vec![
                "-device".to_string(),
                "virtio-blk-pci,drive=disk0".to_string(),
                "-drive".to_string(),
                "/tmp/not-disk0.img".to_string(),
                "-drive".to_string(),
                format!(
                    "id=disk0,if=none,format=raw,file={}",
                    managed_rootfs.display()
                ),
            ],
            ..Default::default()
        };

        let rootfs =
            Starry::qemu_case_rootfs_path(root.path(), &qemu, Path::new("/tmp/default.img"));

        assert_eq!(rootfs, managed_rootfs);
    }

    #[test]
    fn qemu_case_rootfs_ignores_non_managed_drive_file_arg() {
        let root = tempdir().unwrap();
        let qemu = QemuConfig {
            args: vec![
                "-drive".to_string(),
                format!(
                    "id=disk0,if=none,format=raw,file={}",
                    root.path()
                        .join("target/x86_64-unknown-none/rootfs-x86_64.img")
                        .display()
                ),
            ],
            ..Default::default()
        };

        let rootfs =
            Starry::qemu_case_rootfs_path(root.path(), &qemu, Path::new("/tmp/default.img"));

        assert_eq!(rootfs, PathBuf::from("/tmp/default.img"));
    }

    #[test]
    fn qemu_case_rootfs_defaults_without_drive_file_arg() {
        let root = tempdir().unwrap();
        let qemu = QemuConfig::default();

        let rootfs =
            Starry::qemu_case_rootfs_path(root.path(), &qemu, Path::new("/tmp/default.img"));

        assert_eq!(rootfs, PathBuf::from("/tmp/default.img"));
    }

    #[test]
    fn qemu_cases_are_grouped_by_build_config() {
        let default_build_config = PathBuf::from("/tmp/default/build-x86_64-unknown-none.toml");
        let smp4_build_config = PathBuf::from("/tmp/smp4/build-x86_64-unknown-none.toml");
        let cases = vec![
            prepared_qemu_case("smoke", default_build_config.clone()),
            prepared_qemu_case("affinity", smp4_build_config.clone()),
            prepared_qemu_case("syscall", default_build_config.clone()),
        ];

        let groups = Starry::group_qemu_cases_by_build_config(&cases);

        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].build_config_path, default_build_config.as_path());
        assert_eq!(
            groups[0]
                .cases
                .iter()
                .map(|case| case.case.name.as_str())
                .collect::<Vec<_>>(),
            vec!["smoke", "syscall"]
        );
        assert_eq!(groups[1].build_config_path, smp4_build_config.as_path());
        assert_eq!(
            groups[1]
                .cases
                .iter()
                .map(|case| case.case.name.as_str())
                .collect::<Vec<_>>(),
            vec!["affinity"]
        );
    }

    #[test]
    fn qemu_test_request_ignores_inherited_smp() {
        let mut request = starry_request(
            PathBuf::from("/tmp/build-riscv64gc-unknown-none-elf.toml"),
            "riscv64",
            "riscv64gc-unknown-none-elf",
        );
        request.smp = Some(1);

        let request = Starry::qemu_test_request(request);

        assert_eq!(request.smp, None);
    }

    #[test]
    fn qemu_group_build_context_uses_group_build_config_over_default_override() {
        let root = tempdir().unwrap();
        let build_config = write_qemu_build_config_with_max_cpu_num(
            root.path(),
            "normal",
            "qemu-smp4",
            "x86_64-unknown-none",
            4,
        );
        let mut request = starry_request(
            PathBuf::from("/tmp/default-build.toml"),
            "x86_64",
            "x86_64-unknown-none",
        );
        request.build_info_override = Some(crate::starry::build::StarryBuildInfo {
            max_cpu_num: Some(1),
            ..crate::starry::build::StarryBuildInfo::default_starry_for_target(
                "x86_64-unknown-none",
            )
        });

        let (_group_request, cargo) =
            Starry::qemu_group_build_context(&request, &build_config).unwrap();

        assert_eq!(cargo.env.get("SMP").map(String::as_str), Some("4"));
        assert!(cargo.features.contains(&"ax-feat/smp".to_string()));
    }

    #[test]
    fn board_test_group_prefers_case_target_build_config() {
        let root = tempdir().unwrap();
        let build = write_starry_board_build_config(
            root.path(),
            "orangepi-5-plus",
            "aarch64-unknown-none-softfloat",
        );
        write_board_test_config(root.path(), "orangepi-5-plus", "smoke", "orangepi-5-plus");

        let groups = discover_board_test_groups(root.path(), "normal", None, None).unwrap();

        assert_eq!(groups[0].build_config_path, build);
    }

    #[test]
    fn board_test_group_rejects_legacy_case_build_config() {
        let root = tempdir().unwrap();
        write_board_test_config(root.path(), "smoke", "smoke", "orangepi-5-plus");
        let legacy = root
            .path()
            .join("test-suit/starryos/normal/smoke/.build-aarch64-unknown-none-softfloat.toml");
        fs::write(&legacy, "").unwrap();

        let err = discover_board_test_groups(root.path(), "normal", None, None)
            .unwrap_err()
            .to_string();

        assert!(err.contains("not under a build wrapper"));
    }

    #[test]
    fn board_test_group_falls_back_to_mapped_board_build_config() {
        let root = tempdir().unwrap();
        let build = write_starry_board_build_config(
            root.path(),
            "orangepi-5-plus",
            "aarch64-unknown-none-softfloat",
        );
        write_board_test_config(root.path(), "orangepi-5-plus", "smoke", "orangepi-5-plus");

        let groups = discover_board_test_groups(root.path(), "normal", None, None).unwrap();

        assert_eq!(groups[0].build_config_path, build);
    }

    #[test]
    fn qemu_summary_lists_passed_and_failed_cases() {
        let report = StarryQemuRunReport {
            group: "normal".to_string(),
            cases: vec![
                StarryQemuCaseReport {
                    name: "smoke".to_string(),
                    outcome: StarryQemuCaseOutcome::Passed,
                    duration: Duration::from_millis(500),
                },
                StarryQemuCaseReport {
                    name: "usb".to_string(),
                    outcome: StarryQemuCaseOutcome::Failed,
                    duration: Duration::from_secs(2),
                },
            ],
            total_duration: Duration::from_secs(3),
        };

        let summary = render_qemu_case_summary(&report);

        assert!(summary.contains("starry normal qemu summary:"));
        assert!(summary.contains("smoke (0.50s)"));
        assert!(summary.contains("usb (2.00s)"));
        assert!(summary.contains("total: 3.00s"));
    }

    #[test]
    fn resolves_stress_alias_as_stress_group() {
        assert_eq!(resolve_qemu_test_group_name(None, true).unwrap(), "stress");
        assert_eq!(
            resolve_qemu_test_group_name(Some("stress"), true).unwrap(),
            "stress"
        );
    }

    #[test]
    fn rejects_conflicting_stress_alias_and_group() {
        let err = resolve_qemu_test_group_name(Some("normal"), true).unwrap_err();

        assert!(err.to_string().contains("`--stress` is equivalent"));
        assert!(err.to_string().contains("`--test-group normal`"));
    }
}
