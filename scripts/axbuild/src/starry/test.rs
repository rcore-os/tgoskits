use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, bail};
use clap::{Args, Subcommand};
use ostool::{board::RunBoardOptions, build::config::Cargo, run::qemu::QemuConfig};

use super::{Starry, board, build, rootfs};
use crate::{
    context::{
        ResolvedStarryRequest, SnapshotPersistence, StarryCliArgs, arch_for_target_checked,
        resolve_starry_arch_and_target, validate_supported_target,
    },
    test::{
        board as board_test, case, case::TestQemuCase, host_http::HostHttpServerGuard,
        qemu as qemu_test, timing,
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
        short = 'c',
        long = "test-case",
        value_name = "CASE",
        help = "Run only one StarryOS QEMU test case"
    )]
    pub test_case: Option<String>,
    #[arg(short = 'l', long, help = "List discovered StarryOS QEMU test cases")]
    pub list: bool,
}

#[derive(Args, Debug, Clone, Default)]
pub struct ArgsTestBoard {
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

impl qemu_test::BuildConfigRef for StarryQemuCase {
    fn build_group(&self) -> &str {
        &self.build_group
    }

    fn build_config_path(&self) -> &Path {
        &self.build_config_path
    }
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
) -> anyhow::Result<Vec<StarryQemuCase>> {
    let test_suite_dir = require_test_suite_dir(workspace_root)?;
    let selection = parse_starry_qemu_case_selection(selected_case);
    if let Some(direct_case) = selection.prefer_direct_case.as_deref()
        && direct_starry_qemu_case_exists(&test_suite_dir, direct_case)?
    {
        return load_qemu_cases_for_selection(
            &test_suite_dir,
            arch,
            target,
            Some(direct_case),
            None,
        );
    }

    load_qemu_cases_for_selection(
        &test_suite_dir,
        arch,
        target,
        selection.parent_case.as_deref(),
        selection.grouped_subcase_filter,
    )
}

fn load_qemu_cases_for_selection(
    test_suite_dir: &Path,
    arch: &str,
    target: &str,
    selected_case: Option<&str>,
    grouped_subcase_filter: Option<BTreeSet<String>>,
) -> anyhow::Result<Vec<StarryQemuCase>> {
    qemu_test::discover_qemu_cases(
        test_suite_dir,
        arch,
        target,
        selected_case,
        "Starry",
        "qemu",
    )?
    .into_iter()
    .map(|case| load_qemu_case(case, grouped_subcase_filter.clone()))
    .collect()
}

fn direct_starry_qemu_case_exists(
    test_suite_dir: &Path,
    selected_case: &str,
) -> anyhow::Result<bool> {
    match qemu_test::discover_all_qemu_cases(test_suite_dir, Some(selected_case), "Starry", "qemu")
    {
        Ok(cases) => Ok(!cases.is_empty()),
        Err(err) if err.kind() == qemu_test::ListQemuCasesErrorKind::UnknownSelectedCase => {
            Ok(false)
        }
        Err(err) => Err(anyhow::Error::new(err)),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StarryQemuCaseSelection {
    parent_case: Option<String>,
    grouped_subcase_filter: Option<BTreeSet<String>>,
    prefer_direct_case: Option<String>,
}

fn parse_starry_qemu_case_selection(selected_case: Option<&str>) -> StarryQemuCaseSelection {
    let Some(selected_case) = selected_case else {
        return StarryQemuCaseSelection {
            parent_case: None,
            grouped_subcase_filter: None,
            prefer_direct_case: None,
        };
    };

    let parts = selected_case.split('/').collect::<Vec<_>>();
    let mapped = match parts.as_slice() {
        [group, subcase]
            if is_starry_qemu_system_group(group)
                && *subcase != "system"
                && !subcase.is_empty() =>
        {
            Some((
                format!("{group}/system"),
                *subcase,
                Some(selected_case.to_string()),
            ))
        }
        [group, "system", subcase] if is_starry_qemu_system_group(group) && !subcase.is_empty() => {
            Some((format!("{group}/system"), *subcase, None))
        }
        _ => None,
    };

    if let Some((parent_case, subcase, prefer_direct_case)) = mapped {
        return StarryQemuCaseSelection {
            parent_case: Some(parent_case),
            grouped_subcase_filter: Some(BTreeSet::from([subcase.to_string()])),
            prefer_direct_case,
        };
    }

    StarryQemuCaseSelection {
        parent_case: Some(selected_case.to_string()),
        grouped_subcase_filter: None,
        prefer_direct_case: None,
    }
}

fn is_starry_qemu_system_group(group: &str) -> bool {
    matches!(group, "qemu-smp1" | "qemu-smp4")
}

fn load_qemu_case(
    case: qemu_test::DiscoveredQemuCase,
    grouped_subcase_filter: Option<BTreeSet<String>>,
) -> anyhow::Result<StarryQemuCase> {
    let build_group = case.build_group;
    let build_config_path = case.build_config_path;
    let mut test_case = qemu_test::load_test_qemu_case_fields(
        case.display_name,
        case.name,
        case.case_dir,
        case.qemu_config_path,
        "Starry",
        true,
    )?;
    if let Some(filter) = grouped_subcase_filter.as_ref() {
        test_case.grouped_subcase_filter =
            Some(resolve_grouped_subcase_filter(&test_case, filter)?);
    }
    Ok(StarryQemuCase {
        case: test_case,
        build_group,
        build_config_path,
    })
}

fn resolve_grouped_subcase_filter(
    case: &TestQemuCase,
    filter: &BTreeSet<String>,
) -> anyhow::Result<BTreeSet<String>> {
    let canonical_names = case
        .subcases
        .iter()
        .map(|subcase| subcase.name.as_str())
        .collect::<BTreeSet<_>>();
    let aliases = grouped_subcase_selector_aliases(case)?;
    let mut resolved = BTreeSet::new();
    let mut missing = Vec::new();
    for requested in filter {
        if canonical_names.contains(requested.as_str()) {
            resolved.insert(requested.clone());
            continue;
        }

        match aliases.get(requested) {
            Some(matches) if matches.len() == 1 => {
                resolved.extend(matches.iter().cloned());
            }
            Some(matches) => bail!(
                "ambiguous Starry qemu grouped subcase selector `{}` for parent case `{}`; \
                 matches: {}",
                requested,
                case.display_name,
                matches.iter().cloned().collect::<Vec<_>>().join(", ")
            ),
            None => missing.push(requested.as_str()),
        }
    }

    if missing.is_empty() {
        return Ok(resolved);
    }

    bail!(
        "unknown Starry qemu grouped subcase(s) {} for parent case `{}`",
        missing.join(", "),
        case.display_name
    )
}

fn grouped_subcase_selector_aliases(
    case: &TestQemuCase,
) -> anyhow::Result<BTreeMap<String, BTreeSet<String>>> {
    let mut aliases = BTreeMap::new();
    for subcase in &case.subcases {
        add_grouped_subcase_selector_alias(&mut aliases, &subcase.name, &subcase.name);
        for alias in grouped_subcase_binary_aliases(subcase)? {
            add_grouped_subcase_selector_alias(&mut aliases, &alias, &subcase.name);
        }
    }
    Ok(aliases)
}

fn add_grouped_subcase_selector_alias(
    aliases: &mut BTreeMap<String, BTreeSet<String>>,
    alias: &str,
    subcase_name: &str,
) {
    aliases
        .entry(alias.to_string())
        .or_default()
        .insert(subcase_name.to_string());
}

fn grouped_subcase_binary_aliases(
    subcase: &case::TestQemuSubcase,
) -> anyhow::Result<BTreeSet<String>> {
    let cmake_lists = subcase.case_dir.join("CMakeLists.txt");
    if !cmake_lists.is_file() {
        return Ok(BTreeSet::new());
    }

    let content = fs::read_to_string(&cmake_lists)
        .with_context(|| format!("failed to read {}", cmake_lists.display()))?;
    Ok(cmake_target_aliases(&content))
}

fn cmake_target_aliases(content: &str) -> BTreeSet<String> {
    let mut aliases = cmake_install_target_names(content);
    aliases.extend(cmake_executable_target_names(content));
    aliases
}

fn cmake_install_target_names(content: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for line in content.lines() {
        let tokens = cmake_line_tokens(line);
        if !tokens
            .first()
            .is_some_and(|token| token.eq_ignore_ascii_case("install"))
        {
            continue;
        }

        let mut collect_targets = false;
        for token in tokens.iter().skip(1) {
            let keyword = token.to_ascii_uppercase();
            if collect_targets {
                if cmake_install_target_boundary(&keyword) {
                    break;
                }
                names.insert(token.clone());
            } else if keyword == "TARGETS" {
                collect_targets = true;
            }
        }
    }
    names
}

fn cmake_executable_target_names(content: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for line in content.lines() {
        let tokens = cmake_line_tokens(line);
        if tokens
            .first()
            .is_some_and(|token| token.eq_ignore_ascii_case("add_executable"))
            && let Some(target) = tokens.get(1)
        {
            names.insert(target.clone());
        }
    }
    names
}

fn cmake_line_tokens(line: &str) -> Vec<String> {
    let line = line.split_once('#').map_or(line, |(code, _)| code);
    line.split(|ch: char| ch.is_ascii_whitespace() || matches!(ch, '(' | ')'))
        .map(|token| token.trim_matches(|ch| matches!(ch, '"' | '\'')))
        .filter(|token| !token.is_empty())
        .map(ToString::to_string)
        .collect()
}

fn cmake_install_target_boundary(keyword: &str) -> bool {
    matches!(
        keyword,
        "ARCHIVE"
            | "BUNDLE"
            | "COMPONENT"
            | "CONFIGURATIONS"
            | "DESTINATION"
            | "EXCLUDE_FROM_ALL"
            | "EXPORT"
            | "FRAMEWORK"
            | "INCLUDES"
            | "LIBRARY"
            | "NAMELINK_COMPONENT"
            | "NAMELINK_ONLY"
            | "NAMELINK_SKIP"
            | "OBJECTS"
            | "OPTIONAL"
            | "PERMISSIONS"
            | "PRIVATE_HEADER"
            | "PUBLIC_HEADER"
            | "RENAME"
            | "RESOURCE"
            | "RUNTIME"
    )
}

pub(crate) fn finalize_qemu_case_run(report: &StarryQemuRunReport) -> anyhow::Result<()> {
    starry_qemu_summary(report).finish_with_total_detail(
        &starry_qemu_suite_name(report),
        "case",
        Some(&format_duration(report.total_duration)),
    )
}

pub(crate) fn discover_board_test_groups(
    workspace_root: &Path,
    selected_case: Option<&str>,
    selected_board: Option<&str>,
) -> anyhow::Result<Vec<StarryBoardTestGroup>> {
    let test_suite_dir = require_test_suite_dir(workspace_root)?;
    let groups = collect_board_test_groups(workspace_root, &test_suite_dir)?;
    board_test::filter_board_test_groups(groups, selected_case, selected_board, "Starry", || {
        format!(
            "no Starry board test groups found under {}",
            test_suite_dir.display()
        )
    })
}

fn require_test_suite_dir(workspace_root: &Path) -> anyhow::Result<PathBuf> {
    let path = test_suite_root(workspace_root);
    if !path.is_dir() {
        bail!("missing Starry test suite directory `{}`", path.display());
    }
    Ok(path)
}

fn test_suite_root(workspace_root: &Path) -> PathBuf {
    workspace_root.join("test-suit").join(STARRY_TEST_SUITE_OS)
}

fn discover_all_qemu_cases_with_archs(
    workspace_root: &Path,
    selected_case: Option<&str>,
) -> qemu_test::ListQemuCasesResult<Vec<qemu_test::ListedQemuCase>> {
    let test_suite_dir = require_test_suite_dir(workspace_root)?;
    let selection = parse_starry_qemu_case_selection(selected_case);
    let selected_case = if selection.grouped_subcase_filter.is_some() {
        match selection.prefer_direct_case.as_deref() {
            Some(direct_case) if direct_starry_qemu_case_exists(&test_suite_dir, direct_case)? => {
                Some(direct_case)
            }
            _ => selection.parent_case.as_deref(),
        }
    } else {
        selected_case
    };
    qemu_test::discover_all_qemu_cases_with_archs(&test_suite_dir, selected_case, "Starry", "qemu")
}

#[cfg(test)]
fn render_qemu_case_summary(report: &StarryQemuRunReport) -> String {
    starry_qemu_summary(report).render(
        &starry_qemu_suite_name(report),
        "case",
        Some(&format_duration(report.total_duration)),
    )
}

fn starry_qemu_summary(report: &StarryQemuRunReport) -> qemu_test::QemuTestSummary {
    let mut summary = qemu_test::QemuTestSummary::default();
    for case in &report.cases {
        match case.outcome {
            StarryQemuCaseOutcome::Passed => {
                summary.pass_with_detail(&case.name, format_duration(case.duration));
            }
            StarryQemuCaseOutcome::Failed => {
                summary.fail_with_detail(&case.name, format_duration(case.duration));
            }
        }
    }
    summary
}

fn starry_qemu_suite_name(report: &StarryQemuRunReport) -> String {
    let _ = report;
    "starry".to_string()
}

fn format_duration(duration: Duration) -> String {
    format!("{:.2}s", duration.as_secs_f64())
}

fn collect_board_test_groups(
    _workspace_root: &Path,
    test_suite_dir: &Path,
) -> anyhow::Result<Vec<StarryBoardTestGroup>> {
    let mut groups = Vec::new();
    for info in board_test::discover_board_case_build_infos(test_suite_dir, "Starry")? {
        let board_file = board::load_board_file(&info.build_config_path).with_context(|| {
            format!(
                "failed to load Starry board build config `{}`",
                info.build_config_path.display()
            )
        })?;
        let arch = arch_for_target_checked(&board_file.target)?.to_string();
        let target = board_file.target;
        groups.push(StarryBoardTestGroup {
            name: info.name,
            board_name: info.board_name,
            arch,
            target,
            build_config_path: info.build_config_path,
            board_test_config_path: info.board_test_config_path,
        });
    }

    Ok(groups)
}

impl Starry {
    pub(super) async fn test_qemu(&mut self, args: ArgsTestQemu) -> anyhow::Result<()> {
        if args.list && args.arch.is_none() && args.target.is_none() {
            let case_names = discover_all_qemu_cases_with_archs(
                self.app.workspace_root(),
                args.test_case.as_deref(),
            )
            .map_err(anyhow::Error::new)?;
            println!(
                "{}",
                qemu_test::render_qemu_case_forest("starry", [("qemu", case_names)])
            );
            return Ok(());
        }

        let (arch, target) =
            parse_test_target(self.app.workspace_root(), &args.arch, &args.target)?;
        let cases = discover_qemu_cases(
            self.app.workspace_root(),
            &arch,
            &target,
            args.test_case.as_deref(),
        )?;
        if args.list {
            let case_names = cases.iter().map(|case| case.case.display_name.as_str());
            println!("{}", qemu_test::render_case_tree("qemu", case_names));
            return Ok(());
        }
        let package = crate::context::STARRY_PACKAGE;

        println!(
            "running starry qemu tests for package {} on arch: {} (target: {})",
            package, arch, target
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
            crate::image::storage::default_rootfs_path(self.app.workspace_root(), &request.arch)?;
        self.app.set_debug_mode(request.debug)?;

        let total = cases.len();
        let suite_started = Instant::now();
        let mut reports = Vec::new();
        let asset_config = starry_case_asset_config();

        let timing_stage = timing::TimingStage::new(
            "starry-qemu",
            [
                ("phase", "prepare-build-groups".to_string()),
                ("arch", arch.clone()),
                ("target", target.clone()),
            ],
        );
        let build_groups = qemu_test::prepare_case_build_groups(&cases, |build_config_path| {
            Self::qemu_group_build_context(&request, build_config_path)
        });
        timing_stage.finish();
        let build_groups = build_groups?;

        let mut completed = 0;
        for build_group in &build_groups {
            let timing_stage = timing::TimingStage::new(
                "starry-qemu",
                [
                    ("build_group", build_group.group.build_group.to_string()),
                    ("phase", "build".to_string()),
                ],
            );
            let build_result = self
                .app
                .build(
                    build_group.cargo.clone(),
                    build_group.request.build_info_path.clone(),
                )
                .await;
            timing_stage.finish();
            build_result.with_context(|| {
                format!(
                    "failed to build Starry qemu test artifact for build group `{}` ({})",
                    build_group.group.build_group,
                    build_group.group.build_config_path.display()
                )
            })?;

            let timing_stage = timing::TimingStage::new(
                "starry-qemu",
                [
                    ("build_group", build_group.group.build_group.to_string()),
                    ("phase", "prepare-qemu-cases".to_string()),
                ],
            );
            let cases = self
                .prepare_qemu_cases(
                    &build_group.request,
                    &build_group.cargo,
                    &default_rootfs_path,
                    &build_group.group.cases,
                )
                .await;
            timing_stage.finish();
            let cases = cases.with_context(|| {
                format!(
                    "failed to load Starry qemu test cases for build group `{}` ({})",
                    build_group.group.build_group,
                    build_group.group.build_config_path.display()
                )
            })?;

            for case in &cases {
                completed += 1;
                let case_name = &case.case.name;
                println!("[{completed}/{total}] starry qemu {case_name}");

                let case_started = Instant::now();
                match self
                    .run_qemu_case(
                        &build_group.request,
                        &build_group.cargo,
                        case,
                        &asset_config,
                    )
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
                        finalize_qemu_case_run(&StarryQemuRunReport {
                            cases: reports,
                            total_duration: suite_started.elapsed(),
                        })?;
                        bail!("starry qemu test aborted: case `{case_name}` failed");
                    }
                }
            }
        }

        finalize_qemu_case_run(&StarryQemuRunReport {
            cases: reports,
            total_duration: suite_started.elapsed(),
        })
    }

    pub(super) async fn test_board(&mut self, args: ArgsTestBoard) -> anyhow::Result<()> {
        let groups = discover_board_test_groups(
            self.app.workspace_root(),
            args.test_case.as_deref(),
            args.board.as_deref(),
        )?;
        if args.list {
            let case_names = board_test::labeled_board_cases(groups);
            println!(
                "{}",
                qemu_test::render_labeled_case_forest("starry", [("board", case_names)])
            );
            return Ok(());
        }

        let mut run_state = board_test::BoardTestRunState::new("starry", groups.len());
        for (index, group) in groups.into_iter().enumerate() {
            let group_label = run_state.start_group(index, &group);
            let board_test_config = group.board_test_config_path.clone();
            let board_test_config_summary = board_test_config.display().to_string();
            if !board_test_config.exists() {
                run_state.fail_group(
                    group_label,
                    anyhow::anyhow!("missing board test config `{board_test_config_summary}`"),
                );
                continue;
            }

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
                Ok(()) => run_state.pass_group(&group_label),
                Err(err) => run_state.fail_group(group_label, err),
            }
        }
        run_state.finish()
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
        cargo: &Cargo,
        default_rootfs_path: &Path,
        cases: &[&StarryQemuCase],
    ) -> anyhow::Result<Vec<PreparedStarryQemuCase>> {
        let mut prepared = Vec::with_capacity(cases.len());
        let mut rootfs_paths = BTreeSet::new();
        for starry_case in cases {
            let timing_stage = timing::TimingStage::new(
                "starry-qemu",
                [
                    ("case", starry_case.case.display_name.clone()),
                    ("phase", "read-qemu-config".to_string()),
                ],
            );
            let qemu_result = self
                .app
                .read_qemu_config_from_path_for_cargo(cargo, &starry_case.case.qemu_config_path)
                .await;
            timing_stage.finish();
            let mut qemu = qemu_result.with_context(|| {
                format!(
                    "failed to read Starry qemu config for case `{}`",
                    starry_case.case.display_name
                )
            })?;
            let timing_stage = timing::TimingStage::new(
                "starry-qemu",
                [
                    ("case", starry_case.case.display_name.clone()),
                    ("phase", "prepare-qemu-config".to_string()),
                ],
            );
            qemu_test::apply_dynamic_x86_64_qemu_boot(&mut qemu, cargo);
            Self::rewrite_qemu_case_managed_rootfs_paths(self.app.workspace_root(), &mut qemu)?;
            let rootfs_path =
                Self::qemu_case_rootfs_path(self.app.workspace_root(), &qemu, default_rootfs_path)?;
            rootfs_paths.insert(rootfs_path.clone());
            rootfs_paths.extend(Self::qemu_case_managed_rootfs_paths(
                self.app.workspace_root(),
                &qemu,
            )?);
            qemu_test::validate_grouped_qemu_commands(&qemu, &starry_case.case, "Starry")?;
            let requirements = Self::qemu_case_requirements(&qemu).with_context(|| {
                format!(
                    "failed to read QEMU requirements for `{}`",
                    starry_case.case.display_name
                )
            })?;
            timing_stage.finish();
            prepared.push(PreparedStarryQemuCase {
                case: starry_case.case.clone(),
                qemu,
                build_group: starry_case.build_group.clone(),
                build_config_path: starry_case.build_config_path.clone(),
                rootfs_path,
                requirements,
            });
        }

        let timing_stage = timing::TimingStage::new(
            "starry-qemu",
            [
                ("phase", "ensure-rootfs-paths".to_string()),
                ("rootfs_count", rootfs_paths.len().to_string()),
            ],
        );
        let ensure_result = self
            .ensure_qemu_case_rootfs_paths(request, default_rootfs_path, &rootfs_paths)
            .await;
        timing_stage.finish();
        ensure_result?;
        Ok(prepared)
    }

    async fn ensure_qemu_case_rootfs_paths(
        &self,
        request: &ResolvedStarryRequest,
        default_rootfs_path: &Path,
        rootfs_paths: &BTreeSet<PathBuf>,
    ) -> anyhow::Result<()> {
        for rootfs_path in rootfs_paths {
            let rootfs_kind = if rootfs_path == default_rootfs_path {
                "default"
            } else {
                "managed"
            };
            let timing_stage = timing::TimingStage::new(
                "starry-qemu",
                [
                    ("phase", "ensure-rootfs-path".to_string()),
                    ("rootfs_kind", rootfs_kind.to_string()),
                    ("rootfs", rootfs_path.display().to_string()),
                ],
            );
            let result = if rootfs_path == default_rootfs_path {
                rootfs::ensure_rootfs_in_tmp_dir(
                    self.app.workspace_root(),
                    &request.arch,
                    &request.target,
                )
                .await
                .map(|_| ())
            } else {
                crate::image::storage::ensure_optional_managed_rootfs(
                    self.app.workspace_root(),
                    &request.arch,
                    Some(rootfs_path),
                )
                .await
            };
            timing_stage.finish();
            result?;
        }
        Ok(())
    }

    fn qemu_case_rootfs_path(
        workspace_root: &Path,
        qemu: &QemuConfig,
        default_rootfs_path: &Path,
    ) -> anyhow::Result<PathBuf> {
        Ok(Self::qemu_case_managed_rootfs_paths(workspace_root, qemu)?
            .into_iter()
            .next()
            .unwrap_or_else(|| default_rootfs_path.to_path_buf()))
    }

    fn qemu_case_managed_rootfs_paths(
        workspace_root: &Path,
        qemu: &QemuConfig,
    ) -> anyhow::Result<Vec<PathBuf>> {
        crate::rootfs::qemu::drive_file_paths(qemu)
            .into_iter()
            .filter_map(|path| {
                crate::image::storage::resolve_managed_rootfs_path(workspace_root, &path)
                    .transpose()
            })
            .collect()
    }

    fn rewrite_qemu_case_managed_rootfs_paths(
        workspace_root: &Path,
        qemu: &mut QemuConfig,
    ) -> anyhow::Result<()> {
        crate::rootfs::qemu::rewrite_drive_file_paths(qemu, |path| {
            crate::image::storage::resolve_managed_rootfs_path(workspace_root, path)
        })
    }

    fn qemu_case_requirements(qemu: &QemuConfig) -> anyhow::Result<StarryQemuCaseRequirements> {
        Ok(StarryQemuCaseRequirements {
            smp: qemu_test::smp_from_qemu_arg(qemu).unwrap_or(1),
        })
    }

    fn qemu_group_build_context(
        request: &ResolvedStarryRequest,
        build_config_path: &Path,
    ) -> anyhow::Result<(ResolvedStarryRequest, Cargo)> {
        let request = Self::request_for_qemu_case_build_config(request, build_config_path);
        let cargo = build::load_cargo_config(&request)?;

        Ok((request, cargo))
    }

    fn request_for_qemu_case_build_config(
        request: &ResolvedStarryRequest,
        build_config_path: &Path,
    ) -> ResolvedStarryRequest {
        let mut request = request.clone();
        request.build_info_path = build_config_path.to_path_buf();
        request.build_info_override = None;
        request.plat_dyn = None;
        request
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
        asset_config: &case::CaseAssetConfig,
    ) -> anyhow::Result<()> {
        let case = &prepared_case.case;
        let mut qemu = prepared_case.qemu.clone();
        case::apply_grouped_qemu_config(&mut qemu, case, &asset_config.grouped_runner);

        qemu_test::apply_smp_qemu_arg(&mut qemu, Some(prepared_case.requirements.smp));
        qemu_test::apply_timeout_scale(&mut qemu);

        let case_name = &case.name;
        let auto_symbolize =
            crate::build::build_info_enables_backtrace_path(&prepared_case.build_config_path);
        if !case.host_symbolize_success_regex.is_empty() && !auto_symbolize {
            bail!(
                "Starry qemu case `{case_name}` requests host symbolize assertions but its build \
                 config does not enable BACKTRACE=y or DWARF=y"
            );
        }

        let keep_qemu_log = crate::backtrace::keep_qemu_log_from_env();
        let elf = crate::backtrace::std_test_elf_path(
            self.app.workspace_root(),
            &request.target,
            crate::context::STARRY_PACKAGE,
            request.debug,
        );
        let stream_session = if auto_symbolize {
            crate::backtrace::BacktraceSymbolizeSession::try_new(&elf, case_name)
        } else {
            None
        };
        let capture_backtrace = if auto_symbolize {
            let dir = crate::context::axbuild_tmp_dir(self.app.workspace_root()).join("qemu-logs");
            fs::create_dir_all(&dir)?;
            Some(crate::backtrace::BacktraceQemuCapture {
                log_path: dir.join(format!("starry-{case_name}-{}.log", request.target)),
                stream_symbolize: stream_session.clone(),
                suppress_terminal_raw_blocks: false,
                write_log_during_capture: keep_qemu_log,
                captured_blocks: Arc::new(std::sync::Mutex::new(Vec::new())),
            })
        } else {
            None
        };
        let log_path = capture_backtrace
            .as_ref()
            .map(|capture| capture.log_path.clone());
        let memory_blocks = capture_backtrace
            .as_ref()
            .map(|capture| capture.captured_blocks.clone());

        let prepare_stage = timing::TimingStage::new(
            "qemu-case",
            [
                ("case", case.display_name.clone()),
                ("phase", "prepare-assets".to_string()),
            ],
        );
        let prepared_assets_result = case::prepare_case_assets(
            self.app.workspace_root(),
            &request.arch,
            &request.target,
            case,
            prepared_case.rootfs_path.clone(),
            asset_config.clone(),
        )
        .await;
        if prepared_assets_result.is_err() {
            timing::print_timing_line(
                "qemu-case",
                &[
                    ("case", case.display_name.clone()),
                    ("phase", "prepare-assets".to_string()),
                    ("status", "failed".to_string()),
                ],
                prepare_stage.elapsed(),
            );
        }
        let prepared_assets = prepared_assets_result?;
        let prepare_elapsed = prepare_stage.elapsed();
        timing::print_timing_line(
            "qemu-case",
            &[
                ("case", case.display_name.clone()),
                ("phase", "prepare-assets".to_string()),
                ("pipeline", prepared_assets.pipeline.as_str().to_string()),
                (
                    "cache",
                    if prepared_assets.cache_hit {
                        "hit"
                    } else {
                        "miss"
                    }
                    .to_string(),
                ),
            ],
            prepare_elapsed,
        );
        let timing_stage = timing::TimingStage::new(
            "qemu-case",
            [
                ("case", case.display_name.clone()),
                ("phase", "patch-rootfs".to_string()),
            ],
        );
        rootfs::patch_rootfs(
            &mut qemu,
            &prepared_assets.rootfs_path,
            rootfs::RootfsPatchMode::EnsureDiskBootNet,
        );
        timing_stage.finish();
        qemu.args.extend(prepared_assets.extra_qemu_args.clone());
        let timing_stage = timing::TimingStage::new(
            "qemu-case",
            [
                ("case", case.display_name.clone()),
                ("phase", "apply-dynamic-boot".to_string()),
            ],
        );
        qemu_test::apply_dynamic_x86_64_qemu_boot(&mut qemu, cargo);
        timing_stage.finish();
        let timing_stage = timing::TimingStage::new(
            "qemu-case",
            [
                ("case", case.display_name.clone()),
                ("phase", "start-host-http".to_string()),
            ],
        );
        let host_http_result = start_qemu_case_host_http_server(case);
        timing_stage.finish();
        let _host_http_server = host_http_result?;
        case::run_qemu_with_prepared_case_assets(
            &mut self.app,
            cargo,
            qemu,
            capture_backtrace,
            &case.qemu_config_path,
            prepared_assets,
            case::RunPreparedQemuCaseOptions {
                prepare_elapsed,
                qemu_timing_fields: Some(vec![("case", case.display_name.clone())]),
            },
        )
        .await?;

        if auto_symbolize && let Some(path) = log_path {
            let blocks_snapshot = memory_blocks.and_then(|arc| arc.lock().ok().map(|b| b.clone()));
            let symbolized_output = if !case.host_symbolize_success_regex.is_empty() {
                match blocks_snapshot.as_deref() {
                    Some(blocks) => crate::backtrace::symbolize_captured_blocks_to_string(
                        &elf, case_name, blocks,
                    )?,
                    None => None,
                }
            } else {
                None
            };
            let blocks_ref = blocks_snapshot.as_deref();
            let outcome = crate::backtrace::maybe_symbolize_after_qemu(
                &elf,
                &path,
                case_name,
                keep_qemu_log,
                stream_session.as_deref(),
                blocks_ref,
            )?;

            if !case.host_symbolize_success_regex.is_empty() {
                ensure_host_symbolize_output_matches(
                    case_name,
                    outcome,
                    symbolized_output.as_deref(),
                    &case.host_symbolize_success_regex,
                )?;
            }
        }

        Ok(())
    }
}

fn start_qemu_case_host_http_server(
    case: &TestQemuCase,
) -> anyhow::Result<Option<HostHttpServerGuard>> {
    case.host_http_server
        .as_ref()
        .filter(|config| grouped_subcase_needs_host_http_server(case, config))
        .map(|config| HostHttpServerGuard::start(config, &case.name))
        .transpose()
}

fn grouped_subcase_needs_host_http_server(
    case: &TestQemuCase,
    config: &case::HostHttpServerConfig,
) -> bool {
    let Some(filter) = case
        .grouped_subcase_filter
        .as_ref()
        .filter(|filter| !filter.is_empty())
    else {
        return true;
    };

    case.subcases
        .iter()
        .filter(|subcase| filter.contains(subcase.name.as_str()))
        .any(|subcase| subcase_dir_references_host_http_server(&subcase.case_dir, config))
}

fn subcase_dir_references_host_http_server(
    subcase_dir: &Path,
    config: &case::HostHttpServerConfig,
) -> bool {
    if !subcase_dir.is_dir() {
        return false;
    }

    for entry in walkdir::WalkDir::new(subcase_dir)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| should_scan_subcase_http_reference_path(entry.path()))
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
    {
        if file_references_host_http_server(entry.path(), config) {
            return true;
        }
    }

    false
}

fn should_scan_subcase_http_reference_path(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    !matches!(
        name,
        ".git" | "build" | "target" | "CMakeFiles" | "__pycache__"
    )
}

fn file_references_host_http_server(path: &Path, config: &case::HostHttpServerConfig) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if metadata.len() > 1024 * 1024 {
        return false;
    }

    let Ok(mut file) = fs::File::open(path) else {
        return false;
    };
    let mut content = String::new();
    if file.read_to_string(&mut content).is_err() {
        return false;
    }

    let port = config.port.to_string();
    (content.contains("10.0.2.2") && content.contains(&port))
        || content.contains(&format!("http://{}:{port}", config.bind))
        || content.contains(&format!("http://localhost:{port}"))
        || content.contains(&format!("http://127.0.0.1:{port}"))
}

fn ensure_host_symbolize_output_matches(
    case_name: &str,
    outcome: crate::backtrace::SymbolizeAfterQemuOutcome,
    output: Option<&str>,
    regexes: &[String],
) -> anyhow::Result<()> {
    if outcome != crate::backtrace::SymbolizeAfterQemuOutcome::Symbolized {
        bail!("host backtrace symbolize did not run for Starry qemu case `{case_name}`");
    }
    let output =
        output.ok_or_else(|| anyhow::anyhow!("host backtrace symbolize produced no output"))?;
    for pattern in regexes {
        let regex = regex::Regex::new(pattern)
            .with_context(|| format!("invalid host_symbolize_success_regex `{pattern}`"))?;
        if !regex.is_match(output) {
            bail!(
                "host backtrace symbolize output for Starry qemu case `{case_name}` did not match \
                 `{pattern}`"
            );
        }
    }
    Ok(())
}

pub(crate) fn starry_case_asset_config() -> case::CaseAssetConfig {
    case::CaseAssetConfig {
        grouped_runner: case::GroupedCaseRunnerConfig {
            runner_name: "starry-run-case-tests".to_string(),
            runner_path: "/usr/bin/starry-run-case-tests".to_string(),
            autorun_profile_script: Some("99-starry-run-case-tests.sh".to_string()),
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
    use crate::test::case::TestQemuSubcaseKind;

    fn write_qemu_build_config(
        root: &Path,
        _group: &str,
        build_group: &str,
        target: &str,
    ) -> PathBuf {
        let path = root
            .join("test-suit/starryos")
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

    fn write_flat_qemu_build_config(root: &Path, build_group: &str, target: &str) -> PathBuf {
        let path = root
            .join("test-suit/starryos")
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

    fn write_qemu_build_config_with_max_cpu_num(
        root: &Path,
        _group: &str,
        build_group: &str,
        target: &str,
        max_cpu_num: usize,
    ) -> PathBuf {
        let path = root
            .join("test-suit/starryos")
            .join(build_group)
            .join(format!("build-{target}.toml"));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            format!(
                "target = \"{target}\"\nenv = {{}}\nfeatures = [\"qemu\"]\nlog = \
                 \"Info\"\nplat_dyn = true\nmax_cpu_num = {max_cpu_num}\n"
            ),
        )
        .unwrap();
        path
    }

    fn write_starry_board_build_config(root: &Path, build_group: &str, target: &str) -> PathBuf {
        let path = root
            .join("test-suit/starryos")
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
            .join("test-suit/starryos")
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

        let groups = discover_board_test_groups(root.path(), None, None).unwrap();

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
        let case_dir = root.path().join("test-suit/starryos/smoke");
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

        let groups = discover_board_test_groups(root.path(), None, None).unwrap();

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

        let groups = discover_board_test_groups(root.path(), Some("smoke"), None).unwrap();

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
            discover_board_test_groups(root.path(), None, Some("orangepi-5-plus")).unwrap();

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

        let err = discover_board_test_groups(root.path(), None, Some("unknown")).unwrap_err();

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

        let err = discover_board_test_groups(root.path(), None, None)
            .unwrap_err()
            .to_string();

        assert!(err.contains("not under a build wrapper"));
        assert!(err.contains("smoke"));
    }

    fn write_qemu_test_config(
        root: &Path,
        _group: &str,
        build_group: &str,
        case_name: &str,
        arch: &str,
    ) {
        let path = root
            .join("test-suit/starryos")
            .join(build_group)
            .join(case_name)
            .join(format!("qemu-{arch}.toml"));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "timeout = 1\n").unwrap();
    }

    fn write_grouped_qemu_test_config(
        root: &Path,
        _group: &str,
        build_group: &str,
        case_name: &str,
        arch: &str,
    ) {
        let path = root
            .join("test-suit/starryos")
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

    fn write_flat_grouped_qemu_test_config(
        root: &Path,
        build_group: &str,
        case_name: &str,
        arch: &str,
    ) {
        let path = root
            .join("test-suit/starryos")
            .join(build_group)
            .join(case_name)
            .join(format!("qemu-{arch}.toml"));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            path,
            "shell_prefix = \"root@starry:\"\ntest_commands = \
             [\"/usr/bin/starry-run-all\"]\ntimeout = 1\n",
        )
        .unwrap();
    }

    #[test]
    fn inotifywait_qemu_case_installs_tool_before_boot() {
        let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let case_dir = workspace_root.join("apps/starry/qemu/inotifywait");
        let config_path = case_dir.join("qemu-x86_64.toml");
        let cmake_path = case_dir.join("c/CMakeLists.txt");
        let script_path = case_dir.join("c/inotifywait-tests.sh");
        let prebuild_path = case_dir.join("c/prebuild.sh");

        assert!(
            script_path.is_file(),
            "{} must be installed through the case C pipeline",
            script_path.display()
        );
        assert!(
            cmake_path.is_file(),
            "{} must install inotifywait assets through the host CMake install phase",
            cmake_path.display()
        );
        assert!(
            prebuild_path.is_file(),
            "{} must install inotify-tools into the staging root before CMake install",
            prebuild_path.display()
        );

        let script = fs::read_to_string(&script_path).unwrap();
        for guest_apk_command in ["apk update", "apk add"] {
            assert!(
                !script.contains(guest_apk_command),
                "{} must not run `{guest_apk_command}` after StarryOS boots",
                script_path.display()
            );
        }
        assert!(
            script.contains("command -v inotifywait"),
            "{} must still exercise the inotifywait userspace tool",
            script_path.display()
        );

        let prebuild = fs::read_to_string(&prebuild_path).unwrap();
        assert!(
            prebuild.contains("apk add") && prebuild.contains("inotify-tools"),
            "{} must install the inotify-tools package during case asset preparation",
            prebuild_path.display()
        );
        for host_overlay_command in ["STARRY_CASE_OVERLAY_DIR", "cp ", "chmod ", "mkdir "] {
            assert!(
                !prebuild.contains(host_overlay_command),
                "{} must not manipulate host overlay paths from the guest prebuild shell",
                prebuild_path.display()
            );
        }
        assert!(
            prebuild.contains("STARRY_STAGING_ROOT/usr/bin/inotifywait"),
            "{} must verify that apk installed the inotifywait tool in the staging root",
            prebuild_path.display()
        );

        let cmake = fs::read_to_string(&cmake_path).unwrap();
        assert!(
            cmake.contains("install(PROGRAMS inotifywait-tests.sh")
                && cmake.contains("${STARRY_STAGING_ROOT}/usr/bin/inotifywait")
                && cmake.contains("DESTINATION usr/bin"),
            "{} must copy both test script and inotifywait through CMake install",
            cmake_path.display()
        );

        let content = fs::read_to_string(&config_path).unwrap();
        let config: toml::Value = toml::from_str(&content).unwrap();
        let timeout = config
            .get("timeout")
            .and_then(toml::Value::as_integer)
            .unwrap_or_default();
        assert!(
            timeout <= 180,
            "{} must fail quickly because apk setup happens before QEMU boot",
            config_path.display()
        );

        let success_regex = config
            .get("success_regex")
            .and_then(toml::Value::as_array)
            .unwrap();
        assert!(
            success_regex
                .iter()
                .filter_map(toml::Value::as_str)
                .any(|regex| regex.contains("INOTIFYWAIT_TEST_PASSED")),
            "{} must require the inotifywait test pass marker",
            config_path.display()
        );
    }

    #[test]
    fn procps_qemu_case_installs_tools_before_boot() {
        let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let case_dir = workspace_root.join("apps/starry/qemu/procps");
        let cmake_path = case_dir.join("c/CMakeLists.txt");
        let script_path = case_dir.join("c/procps-test.sh");
        let prebuild_path = case_dir.join("c/prebuild.sh");

        assert!(
            script_path.is_file(),
            "{} must be installed through the case C pipeline",
            script_path.display()
        );
        assert!(
            cmake_path.is_file(),
            "{} must install procps assets through the host CMake install phase",
            cmake_path.display()
        );
        assert!(
            prebuild_path.is_file(),
            "{} must install procps into the staging root before CMake install",
            prebuild_path.display()
        );
        assert!(
            !case_dir.join("sh").exists(),
            "{} must not keep the old shell pipeline that cannot prebuild packages",
            case_dir.join("sh").display()
        );

        let script = fs::read_to_string(&script_path).unwrap();
        for guest_apk_command in ["apk update", "apk add", "apk info"] {
            assert!(
                !script.contains(guest_apk_command),
                "{} must not run `{guest_apk_command}` after StarryOS boots",
                script_path.display()
            );
        }
        assert!(
            script.contains("PROCPS_TEST_PASSED") && script.contains("command -v pmap"),
            "{} must still exercise the installed procps tools",
            script_path.display()
        );

        let prebuild = fs::read_to_string(&prebuild_path).unwrap();
        assert!(
            prebuild.contains("apk add") && prebuild.contains("procps"),
            "{} must install procps during case asset preparation",
            prebuild_path.display()
        );
        for tool in ["ps", "free", "uptime", "pgrep", "pmap"] {
            assert!(
                prebuild.contains(tool),
                "{} must verify that apk installed the {tool} tool in the staging root",
                prebuild_path.display()
            );
        }

        let cmake = fs::read_to_string(&cmake_path).unwrap();
        assert!(
            cmake.contains("install(PROGRAMS procps-test.sh")
                && cmake.contains("STARRY_STAGING_ROOT")
                && cmake.contains("ps")
                && cmake.contains("pmap"),
            "{} must install both the procps test script and staging-root tools",
            cmake_path.display()
        );

        for arch in ["aarch64", "loongarch64", "riscv64", "x86_64"] {
            let config_path = case_dir.join(format!("qemu-{arch}.toml"));
            let content = fs::read_to_string(&config_path).unwrap();
            let config: toml::Value = toml::from_str(&content).unwrap();
            let timeout = config
                .get("timeout")
                .and_then(toml::Value::as_integer)
                .unwrap_or_default();
            assert!(
                timeout <= 180,
                "{} must fail quickly because procps setup happens before QEMU boot",
                config_path.display()
            );
        }
    }

    #[test]
    fn apk_add_fs_equivalence_qemu_case_covers_package_fs_ops() {
        let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let case_dir = workspace_root.join("apps/starry/qemu/apk-add-fs-equivalence");
        let cmake_path = case_dir.join("c/CMakeLists.txt");
        let source_path = case_dir.join("c/src/main.c");

        assert!(
            cmake_path.is_file(),
            "{} must build the filesystem equivalence probe through the C pipeline",
            cmake_path.display()
        );
        assert!(
            source_path.is_file(),
            "{} must contain the filesystem equivalence probe source",
            source_path.display()
        );
        assert!(
            !case_dir.join("sh").exists() && !case_dir.join("python").exists(),
            "{} must stay a single C pipeline case",
            case_dir.display()
        );

        let source = fs::read_to_string(&source_path).unwrap();
        for forbidden in [
            "apk update",
            "apk add",
            "apt update",
            "apt install",
            "curl ",
        ] {
            assert!(
                !source.contains(forbidden),
                "{} must not depend on package managers or network clients",
                source_path.display()
            );
        }
        for forbidden in ["http://", "https://"] {
            assert!(
                !source.contains(forbidden),
                "{} must not access external network resources",
                source_path.display()
            );
        }
        for required in [
            "mkdir(",
            "mkdirat(",
            "stat(",
            "lstat(",
            "fstatat(",
            "opendir(",
            "readdir(",
            "open(",
            "O_CREAT",
            "O_TRUNC",
            "O_EXCL",
            "write(",
            "read(",
            "pread(",
            "pwrite(",
            "payload_checksum_update(",
            "rename(",
            "unlink(",
            "chmod(",
            "fchmod(",
            "chown(",
            "fchown(",
            "lchown(",
            "truncate(",
            "ftruncate(",
            "utimensat(",
            "symlink(",
            "readlink(",
            "link(",
            "fsync(",
            "fdatasync(",
            "sync()",
            "syncfs(",
            "APK_ADD_FS_EQUIV_LARGE_PAYLOAD_WRITE_BYTES",
            "APK_ADD_FS_EQUIV_LARGE_PAYLOAD_READ_BYTES",
            "read_checksum == write_checksum",
            "APK_ADD_FS_EQUIV_TEST_PASSED",
            "APK_ADD_FS_EQUIV_TEST_FAILED",
        ] {
            assert!(
                source.contains(required),
                "{} must cover `{required}`",
                source_path.display()
            );
        }
        for simulated_path in ["/usr/bin", "/usr/lib", "/lib/apk/db", "/var/lib/dpkg"] {
            assert!(
                source.contains(simulated_path),
                "{} must simulate package install path `{simulated_path}`",
                source_path.display()
            );
        }

        let cmake = fs::read_to_string(&cmake_path).unwrap();
        assert!(
            cmake.contains("project(apk-add-fs-equivalence C)")
                && cmake.contains("add_executable(apk-add-fs-equivalence")
                && cmake.contains("install(TARGETS apk-add-fs-equivalence")
                && cmake.contains("DESTINATION usr/bin"),
            "{} must install the C probe into the guest image",
            cmake_path.display()
        );

        for arch in ["x86_64", "riscv64"] {
            let config_path = case_dir.join(format!("qemu-{arch}.toml"));
            assert!(
                config_path.is_file(),
                "{} must exist after local validation for {arch}",
                config_path.display()
            );
            let content = fs::read_to_string(&config_path).unwrap();
            let config: toml::Value = toml::from_str(&content).unwrap();
            let args = config.get("args").and_then(toml::Value::as_array).unwrap();
            let args = args
                .iter()
                .filter_map(toml::Value::as_str)
                .collect::<Vec<_>>();

            assert!(
                args.iter()
                    .any(|arg| arg.contains("virtio-blk-pci,drive=disk0")),
                "{} must exercise virtio-blk",
                config_path.display()
            );
            assert!(
                args.iter().any(|arg| {
                    arg.contains(&format!("rootfs-{arch}-alpine.img"))
                        && arg.contains(".tgos-images")
                }),
                "{} must use the managed Alpine rootfs for {arch}",
                config_path.display()
            );

            assert_eq!(
                config
                    .get("shell_init_cmd")
                    .and_then(toml::Value::as_str)
                    .unwrap(),
                "/usr/bin/apk-add-fs-equivalence"
            );
            let success_regex = config
                .get("success_regex")
                .and_then(toml::Value::as_array)
                .unwrap();
            assert!(
                success_regex
                    .iter()
                    .filter_map(toml::Value::as_str)
                    .any(|regex| regex.contains("APK_ADD_FS_EQUIV_TEST_PASSED")),
                "{} must require the pass marker",
                config_path.display()
            );
            assert!(
                success_regex
                    .iter()
                    .filter_map(toml::Value::as_str)
                    .all(|regex| !regex.contains("APK_ADD_FS_EQUIV_LARGE_PAYLOAD")),
                "{} must not use intermediate payload diagnostics as success markers",
                config_path.display()
            );
            let fail_regex = config
                .get("fail_regex")
                .and_then(toml::Value::as_array)
                .unwrap();
            assert!(
                fail_regex
                    .iter()
                    .filter_map(toml::Value::as_str)
                    .any(|regex| regex.contains("APK_ADD_FS_EQUIV_TEST_FAILED")),
                "{} must fail on the probe failure marker",
                config_path.display()
            );
            let timeout = config
                .get("timeout")
                .and_then(toml::Value::as_integer)
                .unwrap_or_default();
            assert!(
                timeout <= 180,
                "{} must stay a focused diagnostic case",
                config_path.display()
            );
        }
    }

    #[test]
    fn apk_net_equivalence_qemu_case_covers_apk_like_network_ops() {
        let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let case_dir = workspace_root.join("apps/starry/qemu/apk-net-equivalence");
        let cmake_path = case_dir.join("c/CMakeLists.txt");
        let source_path = case_dir.join("c/src/main.c");

        assert!(
            cmake_path.is_file(),
            "{} must build the network equivalence probe through the C pipeline",
            cmake_path.display()
        );
        assert!(
            source_path.is_file(),
            "{} must contain the network equivalence probe source",
            source_path.display()
        );
        assert!(
            !case_dir.join("sh").exists() && !case_dir.join("python").exists(),
            "{} must stay a single C pipeline case",
            case_dir.display()
        );

        let source = fs::read_to_string(&source_path).unwrap();
        for forbidden in ["apk update", "apk add", "curl ", "http://", "https://"] {
            assert!(
                !source.contains(forbidden),
                "{} must not depend on package managers, curl, or external URLs",
                source_path.display()
            );
        }
        for required in [
            "socket(",
            "bind(",
            "getsockname(",
            "sendto(",
            "recvfrom(",
            "listen(",
            "accept(",
            "connect(",
            "send(",
            "recv(",
            "GET /alpine/APKINDEX.tar.gz",
            "GET /alpine/main/x86_64/fake-package.apk",
            "Host: apk.local",
            "Content-Length:",
            "APK_NET_EQUIV_TEST_PASSED",
            "APK_NET_EQUIV_TEST_FAILED",
        ] {
            assert!(
                source.contains(required),
                "{} must cover `{required}`",
                source_path.display()
            );
        }

        let cmake = fs::read_to_string(&cmake_path).unwrap();
        assert!(
            cmake.contains("project(apk-net-equivalence C)")
                && cmake.contains("add_executable(apk-net-equivalence")
                && cmake.contains("install(TARGETS apk-net-equivalence")
                && cmake.contains("DESTINATION usr/bin"),
            "{} must install the C probe into the guest image",
            cmake_path.display()
        );

        for arch in ["x86_64", "riscv64"] {
            let config_path = case_dir.join(format!("qemu-{arch}.toml"));
            assert!(
                config_path.is_file(),
                "{} must exist after local validation for {arch}",
                config_path.display()
            );
            let content = fs::read_to_string(&config_path).unwrap();
            let config: toml::Value = toml::from_str(&content).unwrap();
            let args = config.get("args").and_then(toml::Value::as_array).unwrap();
            let args = args
                .iter()
                .filter_map(toml::Value::as_str)
                .collect::<Vec<_>>();

            assert!(
                args.iter()
                    .any(|arg| arg.contains("virtio-blk-pci,drive=disk0")),
                "{} must exercise virtio-blk",
                config_path.display()
            );
            assert!(
                args.iter()
                    .any(|arg| arg.contains("virtio-net-pci,netdev=net0")),
                "{} must exercise virtio-net",
                config_path.display()
            );
            assert!(
                args.iter().any(|arg| {
                    arg.contains(&format!("rootfs-{arch}-alpine.img"))
                        && arg.contains(".tgos-images")
                }),
                "{} must use the managed Alpine rootfs for {arch}",
                config_path.display()
            );

            assert_eq!(
                config
                    .get("shell_init_cmd")
                    .and_then(toml::Value::as_str)
                    .unwrap(),
                "/usr/bin/apk-net-equivalence"
            );
            let success_regex = config
                .get("success_regex")
                .and_then(toml::Value::as_array)
                .unwrap();
            assert!(
                success_regex
                    .iter()
                    .filter_map(toml::Value::as_str)
                    .any(|regex| regex.contains("APK_NET_EQUIV_TEST_PASSED")),
                "{} must require the pass marker",
                config_path.display()
            );
            let fail_regex = config
                .get("fail_regex")
                .and_then(toml::Value::as_array)
                .unwrap();
            assert!(
                fail_regex
                    .iter()
                    .filter_map(toml::Value::as_str)
                    .any(|regex| regex.contains("APK_NET_EQUIV_TEST_FAILED")),
                "{} must fail on the probe failure marker",
                config_path.display()
            );
            let timeout = config
                .get("timeout")
                .and_then(toml::Value::as_integer)
                .unwrap_or_default();
            assert!(
                timeout <= 180,
                "{} must stay a focused diagnostic case",
                config_path.display()
            );
        }
    }

    #[test]
    fn apk_curl_qemu_case_tries_cernet_before_upstream() {
        let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let case_dir = workspace_root.join("apps/starry/qemu/apk-curl");
        let script_path = case_dir.join("sh/apk-curl-tests.sh");
        let script = fs::read_to_string(&script_path).unwrap();

        for arch in ["aarch64", "loongarch64", "riscv64", "x86_64"] {
            let config_path = case_dir.join(format!("qemu-{arch}.toml"));
            let content = fs::read_to_string(&config_path).unwrap();
            let config: toml::Value = toml::from_str(&content).unwrap();
            let shell_init_cmd = config
                .get("shell_init_cmd")
                .and_then(toml::Value::as_str)
                .unwrap();

            assert!(
                shell_init_cmd == "/usr/bin/apk-curl-tests.sh",
                "{} must run the injected apk-curl script instead of pasting a long shell body",
                config_path.display()
            );
        }

        assert!(
            script.contains("apk --timeout \"$fetch_timeout\" add curl"),
            "{} must install curl dynamically to exercise the apk add path",
            script_path.display()
        );
        assert!(
            script.contains("mirrors.cernet.edu.cn") && script.contains("dl-cdn.alpinelinux.org"),
            "{} must provide Cernet first and upstream as a fallback",
            script_path.display()
        );
        let cernet_index = script.find("mirrors.cernet.edu.cn").unwrap();
        let upstream_index = script.find("dl-cdn.alpinelinux.org").unwrap();
        assert!(
            cernet_index < upstream_index,
            "{} must try Cernet before upstream",
            script_path.display()
        );
        assert!(
            !script.contains("mirrors.aliyun.com")
                && !script.contains("mirrors.tuna.tsinghua.edu.cn")
                && !script.contains("mirrors.ustc.edu.cn"),
            "{} must avoid mirrors that repeatedly timeout in QEMU",
            script_path.display()
        );
        assert!(
            !script.contains("__original__"),
            "{} must use explicit mirror attempts so the selected repository is diagnosable",
            script_path.display()
        );
        assert!(
            script.contains("APK_CURL_REPO_$label")
                && script.contains("APK_CURL_TEST_PASSED")
                && script.contains("APK_CURL_TEST_FAILED"),
            "{} must keep clear pass/fail diagnostics",
            script_path.display()
        );
    }

    #[test]
    fn dhcp_qemu_case_checks_local_dhcp_state_without_external_apk_fetch() {
        let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let build_group_dir = workspace_root.join("apps/starry/qemu");
        let case_dir = workspace_root.join("apps/starry/qemu/dhcp");

        for (arch, target) in [
            ("aarch64", "aarch64-unknown-none-softfloat"),
            ("loongarch64", "loongarch64-unknown-none-softfloat"),
            ("riscv64", "riscv64gc-unknown-none-elf"),
            ("x86_64", "x86_64-unknown-none"),
        ] {
            let build_config_path = build_group_dir.join(format!("build-{target}.toml"));
            let build_content = fs::read_to_string(&build_config_path).unwrap();
            let build_config: toml::Value = toml::from_str(&build_content).unwrap();
            assert_eq!(
                build_config.get("target").and_then(toml::Value::as_str),
                Some(target),
                "{} must target {target}",
                build_config_path.display()
            );

            let config_path = case_dir.join(format!("qemu-{arch}.toml"));
            let content = fs::read_to_string(&config_path).unwrap();
            let config: toml::Value = toml::from_str(&content).unwrap();
            let script = config
                .get("shell_init_cmd")
                .and_then(toml::Value::as_str)
                .unwrap();

            assert!(
                !script.contains("apk update")
                    && !script.contains("apk --timeout")
                    && !script.contains("http://")
                    && !script.contains("https://"),
                "{} must not depend on external APK repositories; DHCP is already local to the \
                 QEMU user network",
                config_path.display()
            );
            for marker in [
                "DHCP_PROBE_BEGIN",
                "DHCP_ADDR_OK",
                "DHCP_RESOLVER_OK",
                "DHCP_TEST_DONE",
                "DHCP_TEST_FAILED",
            ] {
                assert!(
                    script.contains(marker),
                    "{} must print the diagnostic marker `{marker}`",
                    config_path.display()
                );
            }
            for expected in [
                "ifconfig eth0",
                "ip addr show",
                "/etc/resolv.conf",
                "10.0.2.15",
                "10.0.2.3",
            ] {
                assert!(
                    script.contains(expected),
                    "{} must check `{expected}` in the local QEMU DHCP state",
                    config_path.display()
                );
            }

            let fail_regex = config
                .get("fail_regex")
                .and_then(toml::Value::as_array)
                .unwrap();
            assert!(
                fail_regex
                    .iter()
                    .filter_map(toml::Value::as_str)
                    .any(|regex| regex.contains("DHCP_TEST_FAILED")),
                "{} must fail explicitly on the DHCP probe failure marker",
                config_path.display()
            );
            let timeout = config
                .get("timeout")
                .and_then(toml::Value::as_integer)
                .unwrap_or_default();
            assert!(
                timeout <= 120,
                "{} must stay a focused local DHCP diagnostic case",
                config_path.display()
            );
        }
    }

    #[test]
    fn lua_qemu_case_installs_lua_before_boot() {
        let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let case_dir = workspace_root.join("apps/starry/qemu/lua");
        let cmake_path = case_dir.join("c/CMakeLists.txt");
        let script_path = case_dir.join("c/lua-app-tests.sh");
        let prebuild_path = case_dir.join("c/prebuild.sh");

        assert!(
            script_path.is_file(),
            "{} must be installed through the case C pipeline",
            script_path.display()
        );
        assert!(
            cmake_path.is_file(),
            "{} must install Lua assets through the host CMake install phase",
            cmake_path.display()
        );
        assert!(
            prebuild_path.is_file(),
            "{} must install Lua packages into the staging root before QEMU boot",
            prebuild_path.display()
        );
        assert!(
            !case_dir.join("sh").exists(),
            "{} must not keep the old shell pipeline that cannot prebuild packages",
            case_dir.join("sh").display()
        );

        let script = fs::read_to_string(&script_path).unwrap();
        for guest_apk_command in ["apk update", "apk add"] {
            assert!(
                !script.contains(guest_apk_command),
                "{} must not run `{guest_apk_command}` after StarryOS boots",
                script_path.display()
            );
        }
        assert!(
            script.contains("lua5.4 /usr/bin/lua-main.lua alpha beta")
                && script.contains("LUA_APP_TEST_FAILED"),
            "{} must still exercise the Lua runtime and report failures",
            script_path.display()
        );

        let prebuild = fs::read_to_string(&prebuild_path).unwrap();
        assert!(
            prebuild.contains("apk add")
                && prebuild.contains("lua5.4")
                && prebuild.contains("lua5.4-cjson"),
            "{} must install Lua packages during case asset preparation",
            prebuild_path.display()
        );
        for staged_path in [
            "STARRY_STAGING_ROOT/usr/bin/lua5.4",
            "STARRY_STAGING_ROOT/usr/lib/lua/5.4/cjson.so",
        ] {
            assert!(
                prebuild.contains(staged_path),
                "{} must verify {} exists in the staging root",
                prebuild_path.display(),
                staged_path
            );
        }

        let cmake = fs::read_to_string(&cmake_path).unwrap();
        assert!(
            cmake.contains("install(PROGRAMS lua-app-tests.sh")
                && cmake.contains("${STARRY_STAGING_ROOT}/usr/bin/lua5.4")
                && cmake.contains("${STARRY_STAGING_ROOT}/usr/lib/lua/5.4/cjson.so"),
            "{} must install the Lua interpreter, cjson module, and test scripts",
            cmake_path.display()
        );

        for arch in ["aarch64", "riscv64", "x86_64"] {
            let config_path = case_dir.join(format!("qemu-{arch}.toml"));
            let content = fs::read_to_string(&config_path).unwrap();
            let config: toml::Value = toml::from_str(&content).unwrap();
            let timeout = config
                .get("timeout")
                .and_then(toml::Value::as_integer)
                .unwrap_or_default();
            assert!(
                timeout <= 180,
                "{} must fail quickly because Lua setup happens before QEMU boot",
                config_path.display()
            );
        }
    }

    #[test]
    fn bug_ext4_dir_ops_is_in_system_grouped_qemu_case() {
        let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let system_dir = workspace_root.join("test-suit/starryos/qemu-smp1/system");
        let case_dir = system_dir.join("bugfix-bug-ext4-dir-ops");
        assert!(
            case_dir.join("CMakeLists.txt").is_file(),
            "{} must remain a system grouped C subcase",
            case_dir.display()
        );

        for arch in ["aarch64", "loongarch64", "riscv64", "x86_64"] {
            let path = system_dir.join(format!("qemu-{arch}.toml"));
            let content = fs::read_to_string(&path).unwrap();
            let config: toml::Value = toml::from_str(&content).unwrap();
            let test_commands = config
                .get("test_commands")
                .and_then(toml::Value::as_array)
                .unwrap();
            assert!(
                test_commands
                    .iter()
                    .filter_map(toml::Value::as_str)
                    .any(|command| command.contains("/usr/bin/starry-test-suit/*")),
                "{} must scan installed system test binaries",
                path.display()
            );
            let success_regex = config
                .get("success_regex")
                .and_then(toml::Value::as_array)
                .unwrap();
            assert!(
                success_regex
                    .iter()
                    .filter_map(toml::Value::as_str)
                    .any(|regex| regex.contains("STARRY_GROUPED_TESTS_PASSED")),
                "{} must require the system grouped success marker",
                path.display()
            );
            let fail_regex = config
                .get("fail_regex")
                .and_then(toml::Value::as_array)
                .unwrap();

            assert!(
                fail_regex
                    .iter()
                    .filter_map(toml::Value::as_str)
                    .any(|regex| regex.contains("STARRY_GROUPED_TEST_FAILED")),
                "{} must fail when a grouped bugfix command fails",
                path.display()
            );
        }
    }

    #[test]
    fn starry_system_grouped_qemu_configs_report_subcase_timing() {
        let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");

        for group in ["qemu-smp1", "qemu-smp4"] {
            let system_dir = workspace_root.join(format!("test-suit/starryos/{group}/system"));
            for arch in ["aarch64", "loongarch64", "riscv64", "x86_64"] {
                let path = system_dir.join(format!("qemu-{arch}.toml"));
                let content = fs::read_to_string(&path).unwrap();
                let config: toml::Value = toml::from_str(&content).unwrap();
                let test_commands = config
                    .get("test_commands")
                    .and_then(toml::Value::as_array)
                    .unwrap();
                let command = test_commands
                    .iter()
                    .filter_map(toml::Value::as_str)
                    .next()
                    .unwrap_or_default();

                assert!(
                    command.contains("STARRY_SYSTEM_TEST_TIMING_BEGIN"),
                    "{} must start a grouped subcase timing section",
                    path.display()
                );
                assert!(
                    command.contains("STARRY_SYSTEM_TEST_TIMING: elapsed_s="),
                    "{} must report per-subcase elapsed seconds",
                    path.display()
                );
                assert!(
                    command.contains("status=passed bin=")
                        && command.contains("status=failed bin="),
                    "{} must include pass/fail status in timing lines",
                    path.display()
                );
                assert!(
                    command.contains("STARRY_SYSTEM_TEST_TIMING_END"),
                    "{} must end a grouped subcase timing section",
                    path.display()
                );
                let failure_branch = command.find("else\n").unwrap_or_else(|| {
                    panic!(
                        "{} must contain a failure branch for grouped subcases",
                        path.display()
                    )
                });
                let failure_command = &command[failure_branch..];
                let exit_status_position =
                    failure_command.find("exit_status=$?").unwrap_or_else(|| {
                        panic!(
                            "{} must preserve grouped subcase exit status",
                            path.display()
                        )
                    });
                let status_failed_position =
                    failure_command.find("status=failed").unwrap_or_else(|| {
                        panic!("{} must mark failed grouped subcases", path.display())
                    });
                assert!(
                    exit_status_position < status_failed_position,
                    "{} must capture `$?` before assigning shell variables in the failure branch",
                    path.display()
                );
                assert!(
                    command.contains("STARRY_GROUPED_TESTS_PASSED")
                        && command.contains("STARRY_GROUPED_TEST_FAILED"),
                    "{} must keep existing grouped success/fail markers",
                    path.display()
                );
            }
        }
    }

    #[test]
    fn zombie_bugfix_commands_are_in_system_grouped_qemu_case() {
        let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let system_dir = workspace_root.join("test-suit/starryos/qemu-smp1/system");
        let zombie_commands = [
            "/usr/bin/bug-kill-zombie-esrch",
            "/usr/bin/bug-kill-zombie-perm",
            "/usr/bin/bug-zombie-syscalls",
            "/usr/bin/bug-waitid-basic",
        ];

        for command in zombie_commands {
            let name = command.trim_start_matches("/usr/bin/");
            assert!(
                system_dir
                    .join(format!("zombie-bugfix-{name}"))
                    .join("CMakeLists.txt")
                    .is_file(),
                "{} must be built in the system grouped case",
                command
            );
        }

        for arch in ["aarch64", "loongarch64", "riscv64", "x86_64"] {
            let system_path = system_dir.join(format!("qemu-{arch}.toml"));
            let system_content = fs::read_to_string(&system_path).unwrap();
            let system_config: toml::Value = toml::from_str(&system_content).unwrap();
            let system_commands = system_config
                .get("test_commands")
                .and_then(toml::Value::as_array)
                .unwrap();
            assert!(
                system_commands
                    .iter()
                    .filter_map(toml::Value::as_str)
                    .any(|command| command.contains("/usr/bin/starry-test-suit/*")),
                "{} must scan installed system test binaries",
                system_path.display()
            );
        }
    }

    #[test]
    fn tty_bugfix_commands_are_in_system_grouped_qemu_case() {
        let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let system_dir = workspace_root.join("test-suit/starryos/qemu-smp1/system");
        let tty_commands = [
            "/usr/bin/bug-raw-terminal-polling",
            "/usr/bin/bug-tty-cursor-report",
        ];

        for command in tty_commands {
            let name = command.trim_start_matches("/usr/bin/");
            assert!(
                system_dir
                    .join(format!("tty-bugfix-{name}"))
                    .join("CMakeLists.txt")
                    .is_file(),
                "{} must be built in the system grouped case",
                command
            );
        }

        for arch in ["aarch64", "loongarch64", "riscv64", "x86_64"] {
            let system_path = system_dir.join(format!("qemu-{arch}.toml"));
            let system_content = fs::read_to_string(&system_path).unwrap();
            let system_config: toml::Value = toml::from_str(&system_content).unwrap();
            let system_commands = system_config
                .get("test_commands")
                .and_then(toml::Value::as_array)
                .unwrap();
            assert!(
                system_commands
                    .iter()
                    .filter_map(toml::Value::as_str)
                    .any(|command| command.contains("/usr/bin/starry-test-suit/*")),
                "{} must scan installed system test binaries",
                system_path.display()
            );
        }
    }

    #[test]
    fn apk_curl_equivalence_is_in_system_grouped_qemu_case() {
        let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let system_dir = workspace_root.join("test-suit/starryos/qemu-smp1/system");
        let subcase_dir = system_dir.join("apk-curl-equivalence");
        let cmake_path = subcase_dir.join("CMakeLists.txt");
        let prebuild_path = system_dir.join("prebuild.sh");
        let script_path = subcase_dir.join("src/apk-curl-equivalence.sh");

        let cmake = fs::read_to_string(&cmake_path)
            .unwrap_or_else(|err| panic!("failed to read {}: {err}", cmake_path.display()));
        let prebuild = fs::read_to_string(&prebuild_path)
            .unwrap_or_else(|err| panic!("failed to read {}: {err}", prebuild_path.display()));
        let script = fs::read_to_string(&script_path)
            .unwrap_or_else(|err| panic!("failed to read {}: {err}", script_path.display()));

        assert!(
            cmake.contains("set(CURL_BIN")
                && cmake.contains("install(PROGRAMS \"${CURL_BIN}\"")
                && cmake.contains("DESTINATION usr/bin/starry-test-suit")
                && cmake.contains("RENAME apk-curl-equivalence"),
            "{} must install curl and the apk-curl equivalence script into the grouped runner",
            cmake_path.display()
        );
        assert!(
            prebuild.contains("apk add") && prebuild.contains("curl"),
            "{} must install curl into the staging rootfs",
            prebuild_path.display()
        );
        assert!(
            !subcase_dir.join("qemu-x86_64.toml").exists(),
            "{} must not carry its own qemu config; qemu-smp1/system owns runtime config",
            subcase_dir.display()
        );
        assert!(
            script.contains("APK_CURL_EQUIVALENCE_TEST_PASSED")
                && script.contains("APK_CURL_EQUIVALENCE_TEST_FAILED")
                && script.contains("curl --connect-timeout")
                && script.contains("10.0.2.2")
                && script.contains("20971520")
                && script.contains("sha256sum -c")
                && script
                    .contains("48b6fb8f1c2fec38d030604889d674722c4af237733c913b698400b59c9294b4"),
            "{} must download the local 20MiB HTTP fixture, write it to disk, then read it back \
             and compare sha256",
            script_path.display()
        );

        for (arch, port) in [
            ("x86_64", 18380_i64),
            ("aarch64", 18381_i64),
            ("riscv64", 18382_i64),
            ("loongarch64", 18383_i64),
        ] {
            let config_path = system_dir.join(format!("qemu-{arch}.toml"));
            let content = fs::read_to_string(&config_path).unwrap();
            let config: toml::Value = toml::from_str(&content).unwrap();
            let host_http_server = config
                .get("host_http_server")
                .and_then(toml::Value::as_table)
                .unwrap_or_else(|| {
                    panic!(
                        "{} must start a local host HTTP fixture for apk-curl-equivalence",
                        config_path.display()
                    )
                });

            assert_eq!(
                host_http_server.get("bind").and_then(toml::Value::as_str),
                Some("127.0.0.1")
            );
            assert_eq!(
                host_http_server
                    .get("port")
                    .and_then(toml::Value::as_integer),
                Some(port)
            );
            assert_eq!(
                host_http_server
                    .get("body_size")
                    .and_then(toml::Value::as_integer),
                Some(20 * 1024 * 1024)
            );
            assert_eq!(
                host_http_server
                    .get("body_byte")
                    .and_then(toml::Value::as_integer),
                Some(i64::from(b'a'))
            );
        }
    }

    #[test]
    fn starry_qemu_case_starts_host_http_server_from_loaded_config() {
        let root = tempdir().unwrap();
        let case_dir = root.path().join("test-suit/starryos/qemu-smp1/system");
        fs::create_dir_all(&case_dir).unwrap();
        let test_case = TestQemuCase {
            name: "qemu-smp1/system".to_string(),
            display_name: "qemu-smp1/system".to_string(),
            case_dir: case_dir.clone(),
            qemu_config_path: case_dir.join("qemu-x86_64.toml"),
            test_commands: Vec::new(),
            host_symbolize_success_regex: Vec::new(),
            host_http_server: Some(case::HostHttpServerConfig {
                bind: "127.0.0.1".to_string(),
                port: 0,
                body: "fixture".to_string(),
                body_size: Some(4),
                body_byte: b'Z',
            }),
            subcases: Vec::new(),
            grouped_subcase_filter: None,
        };

        let guard = start_qemu_case_host_http_server(&test_case).unwrap();

        assert!(guard.is_some());
    }

    #[test]
    fn starry_qemu_single_subcase_skips_unneeded_host_http_server() {
        let root = tempdir().unwrap();
        let case_dir = root.path().join("test-suit/starryos/qemu-smp1/system");
        let subcase_dir = case_dir.join("syscall-test-uid-gid-re-setters");
        fs::create_dir_all(subcase_dir.join("src")).unwrap();
        fs::write(
            subcase_dir.join("src/main.c"),
            "int main(void) { return 0; }\n",
        )
        .unwrap();
        let test_case = grouped_host_http_test_case(
            &case_dir,
            Some(BTreeSet::from([
                "syscall-test-uid-gid-re-setters".to_string()
            ])),
        );

        let guard = start_qemu_case_host_http_server(&test_case).unwrap();

        assert!(guard.is_none());
    }

    #[test]
    fn starry_qemu_single_subcase_keeps_needed_host_http_server() {
        let root = tempdir().unwrap();
        let case_dir = root.path().join("test-suit/starryos/qemu-smp1/system");
        let subcase_dir = case_dir.join("apk-curl-equivalence");
        fs::create_dir_all(subcase_dir.join("src")).unwrap();
        fs::write(
            subcase_dir.join("src/apk-curl-equivalence.sh"),
            "curl -fsSL http://10.0.2.2:18380/payload.bin\n",
        )
        .unwrap();
        let mut test_case = grouped_host_http_test_case(
            &case_dir,
            Some(BTreeSet::from(["apk-curl-equivalence".to_string()])),
        );
        test_case.host_http_server.as_mut().unwrap().port = 0;

        let guard = start_qemu_case_host_http_server(&test_case).unwrap();

        assert!(guard.is_some());
    }

    fn grouped_host_http_test_case(
        case_dir: &Path,
        grouped_subcase_filter: Option<BTreeSet<String>>,
    ) -> TestQemuCase {
        TestQemuCase {
            name: "qemu-smp1/system".to_string(),
            display_name: "qemu-smp1/system".to_string(),
            case_dir: case_dir.to_path_buf(),
            qemu_config_path: case_dir.join("qemu-x86_64.toml"),
            test_commands: Vec::new(),
            host_symbolize_success_regex: Vec::new(),
            host_http_server: Some(case::HostHttpServerConfig {
                bind: "127.0.0.1".to_string(),
                port: 18380,
                body: "fixture".to_string(),
                body_size: Some(4),
                body_byte: b'Z',
            }),
            subcases: grouped_subcase_filter
                .as_ref()
                .into_iter()
                .flatten()
                .map(|name| case::TestQemuSubcase {
                    name: name.clone(),
                    case_dir: case_dir.join(name),
                    kind: case::TestQemuSubcaseKind::C,
                })
                .collect(),
            grouped_subcase_filter,
        }
    }

    #[test]
    fn busybox_guest_script_reports_case_start_and_bounds_nologin() {
        let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let script_path = workspace_root.join("apps/starry/qemu/busybox/sh/busybox-tests.sh");
        let script = fs::read_to_string(&script_path).unwrap();

        assert!(
            script.contains("echo \"START: $BB_CASE_NAME\""),
            "{} must print case start markers so CI timeout logs identify the hanging BusyBox \
             applet",
            script_path.display()
        );
        assert!(
            script.contains("timeout 2 busybox nologin"),
            "{} must run nologin in the foreground under a timeout",
            script_path.display()
        );
        assert!(
            !script.contains("busybox nologin >/tmp/bb_nologin.out 2>&1 &"),
            "{} must not leave the nologin probe as a background child",
            script_path.display()
        );
    }

    #[test]
    fn starry_grouped_cases_install_profile_autorun() {
        let config = starry_case_asset_config();

        assert_eq!(
            config.grouped_runner.autorun_profile_script.as_deref(),
            Some("99-starry-run-case-tests.sh")
        );
    }

    fn prepared_qemu_case(name: &str, build_config_path: PathBuf) -> PreparedStarryQemuCase {
        PreparedStarryQemuCase {
            case: TestQemuCase {
                name: name.to_string(),
                display_name: name.to_string(),
                case_dir: PathBuf::from(format!("/tmp/{name}")),
                qemu_config_path: PathBuf::from(format!("/tmp/{name}/qemu-x86_64.toml")),
                test_commands: Vec::new(),
                host_symbolize_success_regex: Vec::new(),
                host_http_server: None,
                subcases: Vec::new(),
                grouped_subcase_filter: None,
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
        fs::create_dir_all(root.path().join("test-suit/starryos/default/usb")).unwrap();

        let cases =
            discover_qemu_cases(root.path(), "x86_64", "x86_64-unknown-none", None).unwrap();

        assert_eq!(cases.len(), 1);
        assert_eq!(cases[0].case.name, "smoke");
        assert!(cases[0].case.test_commands.is_empty());
        assert!(cases[0].case.subcases.is_empty());
        assert_eq!(
            cases[0].case.case_dir,
            root.path().join("test-suit/starryos/default/smoke")
        );
    }

    #[test]
    fn discovers_grouped_case_commands_and_sorted_subcases() {
        let root = tempdir().unwrap();
        write_qemu_build_config(root.path(), "normal", "default", "x86_64-unknown-none");
        write_grouped_qemu_test_config(root.path(), "normal", "default", "bugfix", "x86_64");
        fs::create_dir_all(root.path().join("test-suit/starryos/default/bugfix/beta/c")).unwrap();
        fs::create_dir_all(
            root.path()
                .join("test-suit/starryos/default/bugfix/alpha/c"),
        )
        .unwrap();

        let cases =
            discover_qemu_cases(root.path(), "x86_64", "x86_64-unknown-none", None).unwrap();

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
    fn discovers_flat_qemu_wrapper_case_with_subcases() {
        let root = tempdir().unwrap();
        write_flat_qemu_build_config(root.path(), "qemu-smp1", "x86_64-unknown-none");
        write_flat_grouped_qemu_test_config(root.path(), "qemu-smp1", "system", "x86_64");
        let case_dir = root.path().join("test-suit/starryos/qemu-smp1/system");
        fs::create_dir_all(case_dir.join("smoke/c")).unwrap();
        fs::create_dir_all(case_dir.join("usb-storage/c")).unwrap();

        let cases =
            discover_qemu_cases(root.path(), "x86_64", "x86_64-unknown-none", None).unwrap();

        assert_eq!(cases.len(), 1);
        assert_eq!(cases[0].case.name, "system");
        assert_eq!(cases[0].case.display_name, "qemu-smp1/system");
        assert_eq!(cases[0].build_group, "qemu-smp1");
        assert_eq!(
            cases[0]
                .case
                .subcases
                .iter()
                .map(|subcase| subcase.name.as_str())
                .collect::<Vec<_>>(),
            vec!["smoke", "usb-storage"]
        );

        let selected = discover_qemu_cases(
            root.path(),
            "x86_64",
            "x86_64-unknown-none",
            Some("qemu-smp1/system"),
        )
        .unwrap();
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].case.display_name, "qemu-smp1/system");

        let listed = discover_all_qemu_cases_with_archs(root.path(), None).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "qemu-smp1/system");
    }

    #[test]
    fn starry_qemu_subcase_selector_maps_to_system_parent() {
        let root = tempdir().unwrap();
        write_flat_qemu_build_config(root.path(), "qemu-smp1", "x86_64-unknown-none");
        write_flat_grouped_qemu_test_config(root.path(), "qemu-smp1", "system", "x86_64");
        let case_dir = root.path().join("test-suit/starryos/qemu-smp1/system");
        fs::create_dir_all(case_dir.join("alpha/src")).unwrap();
        fs::write(
            case_dir.join("alpha/CMakeLists.txt"),
            "add_executable(alpha src/main.c)\n",
        )
        .unwrap();

        let cases = discover_qemu_cases(
            root.path(),
            "x86_64",
            "x86_64-unknown-none",
            Some("qemu-smp1/alpha"),
        )
        .unwrap();

        assert_eq!(cases.len(), 1);
        assert_eq!(cases[0].case.display_name, "qemu-smp1/system");
        assert_eq!(
            cases[0].case.grouped_subcase_filter,
            Some(BTreeSet::from(["alpha".to_string()]))
        );
    }

    #[test]
    fn starry_qemu_subcase_selector_accepts_installed_binary_name() {
        let root = tempdir().unwrap();
        write_flat_qemu_build_config(root.path(), "qemu-smp1", "x86_64-unknown-none");
        write_flat_grouped_qemu_test_config(root.path(), "qemu-smp1", "system", "x86_64");
        let case_dir = root.path().join("test-suit/starryos/qemu-smp1/system");
        fs::create_dir_all(case_dir.join("syscall-test-uid-gid-re-setters/src")).unwrap();
        fs::write(
            case_dir.join("syscall-test-uid-gid-re-setters/CMakeLists.txt"),
            r#"
add_executable(test-uid-gid-re-setters src/main.c)
install(TARGETS test-uid-gid-re-setters RUNTIME DESTINATION usr/bin/starry-test-suit)
"#,
        )
        .unwrap();

        let cases = discover_qemu_cases(
            root.path(),
            "x86_64",
            "x86_64-unknown-none",
            Some("qemu-smp1/test-uid-gid-re-setters"),
        )
        .unwrap();

        assert_eq!(cases.len(), 1);
        assert_eq!(cases[0].case.display_name, "qemu-smp1/system");
        assert_eq!(
            cases[0].case.grouped_subcase_filter,
            Some(BTreeSet::from([
                "syscall-test-uid-gid-re-setters".to_string()
            ]))
        );
    }

    #[test]
    fn starry_qemu_system_subcase_selector_sets_filter() {
        let root = tempdir().unwrap();
        write_flat_qemu_build_config(root.path(), "qemu-smp4", "x86_64-unknown-none");
        write_flat_grouped_qemu_test_config(root.path(), "qemu-smp4", "system", "x86_64");
        let case_dir = root.path().join("test-suit/starryos/qemu-smp4/system");
        fs::create_dir_all(case_dir.join("test-futex-race/src")).unwrap();
        fs::write(
            case_dir.join("test-futex-race/CMakeLists.txt"),
            "add_executable(test-futex-race src/main.c)\n",
        )
        .unwrap();

        let cases = discover_qemu_cases(
            root.path(),
            "x86_64",
            "x86_64-unknown-none",
            Some("qemu-smp4/system/test-futex-race"),
        )
        .unwrap();

        assert_eq!(cases.len(), 1);
        assert_eq!(cases[0].case.display_name, "qemu-smp4/system");
        assert_eq!(
            cases[0].case.grouped_subcase_filter,
            Some(BTreeSet::from(["test-futex-race".to_string()]))
        );
    }

    #[test]
    fn starry_qemu_system_selector_keeps_full_group() {
        let root = tempdir().unwrap();
        write_flat_qemu_build_config(root.path(), "qemu-smp1", "x86_64-unknown-none");
        write_flat_grouped_qemu_test_config(root.path(), "qemu-smp1", "system", "x86_64");
        let case_dir = root.path().join("test-suit/starryos/qemu-smp1/system");
        fs::create_dir_all(case_dir.join("alpha/src")).unwrap();
        fs::write(
            case_dir.join("alpha/CMakeLists.txt"),
            "add_executable(alpha src/main.c)\n",
        )
        .unwrap();

        let cases = discover_qemu_cases(
            root.path(),
            "x86_64",
            "x86_64-unknown-none",
            Some("qemu-smp1/system"),
        )
        .unwrap();

        assert_eq!(cases.len(), 1);
        assert_eq!(cases[0].case.display_name, "qemu-smp1/system");
        assert_eq!(cases[0].case.grouped_subcase_filter, None);
    }

    #[test]
    fn starry_qemu_subcase_selector_reports_unknown_subcase() {
        let root = tempdir().unwrap();
        write_flat_qemu_build_config(root.path(), "qemu-smp1", "x86_64-unknown-none");
        write_flat_grouped_qemu_test_config(root.path(), "qemu-smp1", "system", "x86_64");
        let case_dir = root.path().join("test-suit/starryos/qemu-smp1/system");
        fs::create_dir_all(case_dir.join("alpha/src")).unwrap();
        fs::write(
            case_dir.join("alpha/CMakeLists.txt"),
            "add_executable(alpha src/main.c)\n",
        )
        .unwrap();

        let err = discover_qemu_cases(
            root.path(),
            "x86_64",
            "x86_64-unknown-none",
            Some("qemu-smp1/missing"),
        )
        .unwrap_err()
        .to_string();

        assert!(err.contains("qemu-smp1/system"));
        assert!(err.contains("missing"));
    }

    #[test]
    fn starry_qemu_subcase_selector_prefers_existing_direct_case() {
        let root = tempdir().unwrap();
        write_flat_qemu_build_config(root.path(), "qemu-smp1", "x86_64-unknown-none");
        write_flat_grouped_qemu_test_config(root.path(), "qemu-smp1", "system", "x86_64");
        write_qemu_test_config(root.path(), "normal", "qemu-smp1", "alpha", "x86_64");
        let case_dir = root.path().join("test-suit/starryos/qemu-smp1/system");
        fs::create_dir_all(case_dir.join("alpha/src")).unwrap();
        fs::write(
            case_dir.join("alpha/CMakeLists.txt"),
            "add_executable(alpha src/main.c)\n",
        )
        .unwrap();

        let cases = discover_qemu_cases(
            root.path(),
            "x86_64",
            "x86_64-unknown-none",
            Some("qemu-smp1/alpha"),
        )
        .unwrap();

        assert_eq!(cases.len(), 1);
        assert_eq!(cases[0].case.display_name, "qemu-smp1/alpha");
        assert_eq!(cases[0].case.grouped_subcase_filter, None);
    }

    #[test]
    fn starry_qemu_list_accepts_subcase_selector() {
        let root = tempdir().unwrap();
        write_flat_qemu_build_config(root.path(), "qemu-smp1", "x86_64-unknown-none");
        write_flat_grouped_qemu_test_config(root.path(), "qemu-smp1", "system", "x86_64");

        let listed =
            discover_all_qemu_cases_with_archs(root.path(), Some("qemu-smp1/alpha")).unwrap();

        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "qemu-smp1/system");
    }

    #[test]
    fn starry_qemu_list_prefers_existing_direct_case() {
        let root = tempdir().unwrap();
        write_flat_qemu_build_config(root.path(), "qemu-smp1", "x86_64-unknown-none");
        write_flat_grouped_qemu_test_config(root.path(), "qemu-smp1", "system", "x86_64");
        write_qemu_test_config(root.path(), "normal", "qemu-smp1", "alpha", "x86_64");

        let listed =
            discover_all_qemu_cases_with_archs(root.path(), Some("qemu-smp1/alpha")).unwrap();

        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "qemu-smp1/alpha");
    }

    #[test]
    fn discovers_flat_qemu_wrapper_case_with_root_cmake_subcases() {
        let root = tempdir().unwrap();
        write_flat_qemu_build_config(root.path(), "qemu-smp1", "x86_64-unknown-none");
        write_flat_grouped_qemu_test_config(root.path(), "qemu-smp1", "system", "x86_64");
        let case_dir = root.path().join("test-suit/starryos/qemu-smp1/system");
        fs::create_dir_all(&case_dir).unwrap();
        fs::write(
            case_dir.join("CMakeLists.txt"),
            "cmake_minimum_required(VERSION 3.20)\nproject(system C)\nadd_subdirectory(smoke)\n",
        )
        .unwrap();
        fs::create_dir_all(case_dir.join("smoke/src")).unwrap();
        fs::write(
            case_dir.join("smoke/CMakeLists.txt"),
            "add_executable(smoke src/main.c)\n",
        )
        .unwrap();

        let cases =
            discover_qemu_cases(root.path(), "x86_64", "x86_64-unknown-none", None).unwrap();

        assert_eq!(cases.len(), 1);
        assert_eq!(
            cases[0]
                .case
                .subcases
                .iter()
                .map(|subcase| subcase.name.as_str())
                .collect::<Vec<_>>(),
            vec!["smoke"]
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
    fn starry_system_grouped_cases_use_root_cmake_layout() {
        let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");

        for group in ["qemu-smp1", "qemu-smp4"] {
            let system_dir = workspace_root.join(format!("test-suit/starryos/{group}/system"));
            let root_cmake = system_dir.join("CMakeLists.txt");
            assert!(
                root_cmake.is_file(),
                "{} must be the grouped system CMake project entry",
                root_cmake.display()
            );

            let mut subcase_count = 0;
            for entry in fs::read_dir(&system_dir).unwrap() {
                let entry = entry.unwrap();
                let path = entry.path();
                if !path.is_dir()
                    || path.file_name().is_some_and(|name| name == "common")
                    || !path.join("CMakeLists.txt").is_file()
                {
                    continue;
                }
                subcase_count += 1;
                assert!(
                    !path.join("c").exists(),
                    "{} must keep CMakeLists.txt and src/ directly under the subcase",
                    path.display()
                );
                assert!(
                    path.join("src").is_dir() || path.join("CMakeLists.txt").is_file(),
                    "{} must remain a buildable subcase directory",
                    path.display()
                );
            }

            assert!(
                subcase_count > 0,
                "{} must contain grouped C subcases",
                system_dir.display()
            );
        }
    }

    #[test]
    fn grouped_case_skips_arch_specific_subcases_for_other_arches() {
        let root = tempdir().unwrap();
        write_qemu_build_config(
            root.path(),
            "normal",
            "default",
            "riscv64gc-unknown-none-elf",
        );
        write_grouped_qemu_test_config(root.path(), "normal", "default", "syscall", "riscv64");

        let case_dir = root.path().join("test-suit/starryos/default/syscall");
        fs::create_dir_all(case_dir.join("alpha/c")).unwrap();
        fs::create_dir_all(case_dir.join("x86-only/c")).unwrap();
        fs::write(case_dir.join("x86-only/qemu-x86_64.toml"), "timeout = 1\n").unwrap();

        let cases = discover_qemu_cases(root.path(), "riscv64", "riscv64gc-unknown-none-elf", None)
            .unwrap();

        assert_eq!(cases.len(), 1);
        assert_eq!(
            cases[0]
                .case
                .subcases
                .iter()
                .map(|subcase| subcase.name.as_str())
                .collect::<Vec<_>>(),
            vec!["alpha"]
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
            .join("test-suit/starryos/default/bugfix/qemu-x86_64.toml");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            "shell_prefix = \"root@starry:\"\nshell_init_cmd = \"/usr/bin/old\"\ntest_commands = \
             [\"/usr/bin/new\"]\n",
        )
        .unwrap();

        // Discovery no longer validates the shell_init_cmd / test_commands
        // conflict; it should succeed and leave a grouped case behind.
        let cases =
            discover_qemu_cases(root.path(), "x86_64", "x86_64-unknown-none", Some("bugfix"))
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
            .join("test-suit/starryos/default/bugfix/qemu-x86_64.toml");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "test_commands = [\"/usr/bin/ok\", \"  \"]\n").unwrap();

        let err = discover_qemu_cases(root.path(), "x86_64", "x86_64-unknown-none", Some("bugfix"))
            .unwrap_err()
            .to_string();

        assert!(err.contains("contains an empty test command"));
    }

    #[test]
    fn selected_case_requires_matching_qemu_config() {
        let root = tempdir().unwrap();
        write_qemu_build_config(root.path(), "normal", "default", "x86_64-unknown-none");
        fs::create_dir_all(root.path().join("test-suit/starryos/default/usb")).unwrap();

        let err = discover_qemu_cases(root.path(), "x86_64", "x86_64-unknown-none", Some("usb"))
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
                .join("test-suit/starryos/board-orangepi-5-plus/smoke"),
        )
        .unwrap();
        fs::write(
            root.path()
                .join("test-suit/starryos/board-orangepi-5-plus/smoke/board-orangepi-5-plus.toml"),
            "board_type = \"OrangePi-5-Plus\"\n",
        )
        .unwrap();
        write_qemu_test_config(root.path(), "normal", "qemu-smp1", "smoke", "x86_64");

        let cases =
            discover_qemu_cases(root.path(), "x86_64", "x86_64-unknown-none", Some("smoke"))
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
        write_test_image_config(root.path());
        let managed_rootfs = root
            .path()
            .join(".tgos-images/rootfs-riscv64-debian.img/rootfs-riscv64-debian.img");
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
            Starry::qemu_case_rootfs_path(root.path(), &qemu, Path::new("/tmp/default.img"))
                .unwrap();

        assert_eq!(rootfs, managed_rootfs);
    }

    #[test]
    fn qemu_case_rootfs_accepts_drive_file_with_additional_options() {
        let root = tempdir().unwrap();
        write_test_image_config(root.path());
        let managed_rootfs = root
            .path()
            .join(".tgos-images/rootfs-aarch64-busybox.img/rootfs-aarch64-busybox.img");
        let qemu = QemuConfig {
            args: vec![
                "-drive".to_string(),
                format!(
                    "id=usbdisk,if=none,format=raw,snapshot=on,file={}",
                    managed_rootfs.display()
                ),
            ],
            ..Default::default()
        };

        let rootfs =
            Starry::qemu_case_rootfs_path(root.path(), &qemu, Path::new("/tmp/default.img"))
                .unwrap();

        assert_eq!(rootfs, managed_rootfs);
    }

    #[test]
    fn qemu_case_rootfs_collects_all_managed_drive_files() {
        let root = tempdir().unwrap();
        write_test_image_config(root.path());
        let boot_rootfs = root
            .path()
            .join(".tgos-images/rootfs-aarch64-alpine.img/rootfs-aarch64-alpine.img");
        let usb_rootfs = root
            .path()
            .join(".tgos-images/rootfs-aarch64-busybox.img/rootfs-aarch64-busybox.img");
        let qemu = QemuConfig {
            args: vec![
                "-drive".to_string(),
                format!("id=disk0,if=none,format=raw,file={}", boot_rootfs.display()),
                "-drive".to_string(),
                format!(
                    "id=usbdisk,if=none,format=raw,snapshot=on,file={}",
                    usb_rootfs.display()
                ),
            ],
            ..Default::default()
        };

        let rootfs_paths = Starry::qemu_case_managed_rootfs_paths(root.path(), &qemu).unwrap();

        assert_eq!(rootfs_paths, vec![boot_rootfs, usb_rootfs]);
    }

    #[test]
    fn qemu_case_rewrites_legacy_tmp_rootfs_drive_files() {
        let root = tempdir().unwrap();
        write_test_image_config(root.path());
        let image_name = "rootfs-aarch64-busybox.img";
        let legacy_rootfs = root.path().join("tmp/axbuild/rootfs").join(image_name);
        let managed_rootfs = root
            .path()
            .join(".tgos-images")
            .join(image_name)
            .join(image_name);
        let mut qemu = QemuConfig {
            args: vec![
                "-drive".to_string(),
                format!(
                    "id=usbdisk,if=none,format=raw,snapshot=on,file={}",
                    legacy_rootfs.display()
                ),
            ],
            ..Default::default()
        };

        Starry::rewrite_qemu_case_managed_rootfs_paths(root.path(), &mut qemu).unwrap();

        assert_eq!(
            qemu.args,
            vec![
                "-drive".to_string(),
                format!(
                    "id=usbdisk,if=none,format=raw,snapshot=on,file={}",
                    managed_rootfs.display()
                ),
            ]
        );
        assert_eq!(
            Starry::qemu_case_managed_rootfs_paths(root.path(), &qemu).unwrap(),
            vec![managed_rootfs]
        );
    }

    #[test]
    fn qemu_case_rootfs_ignores_non_managed_drive_file_arg() {
        let root = tempdir().unwrap();
        write_test_image_config(root.path());
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
            Starry::qemu_case_rootfs_path(root.path(), &qemu, Path::new("/tmp/default.img"))
                .unwrap();

        assert_eq!(rootfs, PathBuf::from("/tmp/default.img"));
    }

    #[test]
    fn qemu_case_rootfs_defaults_without_drive_file_arg() {
        let root = tempdir().unwrap();
        write_test_image_config(root.path());
        let qemu = QemuConfig::default();

        let rootfs =
            Starry::qemu_case_rootfs_path(root.path(), &qemu, Path::new("/tmp/default.img"))
                .unwrap();

        assert_eq!(rootfs, PathBuf::from("/tmp/default.img"));
    }

    fn write_test_image_config(workspace_root: &Path) {
        let config = crate::image::config::ImageConfig {
            local_storage: workspace_root.join(".tgos-images"),
            registry: crate::image::config::DEFAULT_REGISTRY_URL.to_string(),
            auto_sync: true,
            auto_sync_threshold: 60,
        };
        crate::image::config::ImageConfig::write_config(workspace_root, &config).unwrap();
    }

    #[test]
    fn qemu_cases_are_grouped_by_build_config() {
        let default_build_config = PathBuf::from("/tmp/default/build-x86_64-unknown-none.toml");
        let smp4_build_config = PathBuf::from("/tmp/smp4/build-x86_64-unknown-none.toml");
        let cases = vec![
            prepared_qemu_case("smoke", default_build_config.clone()),
            prepared_qemu_case("qemu-smp4/system", smp4_build_config.clone()),
            prepared_qemu_case("syscall", default_build_config.clone()),
        ];

        let groups = qemu_test::group_cases_by_build_config(&cases);

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
            vec!["qemu-smp4/system"]
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
            plat_dyn: true,
            ..crate::starry::build::default_starry_build_info_for_target("x86_64-unknown-none")
        });

        let (_group_request, cargo) =
            Starry::qemu_group_build_context(&request, &build_config).unwrap();

        assert_eq!(cargo.env.get("SMP").map(String::as_str), Some("4"));
        assert!(cargo.features.contains(&"ax-std/smp".to_string()));
    }

    #[test]
    fn qemu_group_build_context_uses_group_plat_dyn_over_default_request() {
        let root = tempdir().unwrap();
        let build_config = root
            .path()
            .join("test-suit/starryos/qemu-smp1/build-aarch64-unknown-none-softfloat.toml");
        fs::create_dir_all(build_config.parent().unwrap()).unwrap();
        fs::write(
            &build_config,
            "target = \"aarch64-unknown-none-softfloat\"\nenv = {}\nfeatures = [\"qemu\"]\nlog = \
             \"Warn\"\nplat_dyn = true\n",
        )
        .unwrap();
        let mut request = starry_request(
            PathBuf::from("/tmp/default-build.toml"),
            "aarch64",
            "aarch64-unknown-none-softfloat",
        );
        request.plat_dyn = Some(false);
        request.build_info_override = Some(crate::starry::build::StarryBuildInfo {
            features: vec!["qemu".to_string()],
            plat_dyn: false,
            ..crate::starry::build::default_starry_build_info_for_target(
                "aarch64-unknown-none-softfloat",
            )
        });

        let (_group_request, cargo) =
            Starry::qemu_group_build_context(&request, &build_config).unwrap();

        assert!(cargo.features.contains(&"plat-dyn".to_string()));
        assert!(!cargo.features.contains(&"ax-feat/plat-dyn".to_string()));
        assert!(
            !cargo
                .features
                .contains(&"starry-kernel/plat-dyn".to_string())
        );
        assert!(!cargo.features.contains(&"qemu".to_string()));
        assert!(
            cargo
                .target
                .ends_with("scripts/targets/std/pie/aarch64-unknown-linux-musl.json")
        );
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

        let groups = discover_board_test_groups(root.path(), None, None).unwrap();

        assert_eq!(groups[0].build_config_path, build);
    }

    #[test]
    fn board_test_group_rejects_legacy_case_build_config() {
        let root = tempdir().unwrap();
        write_board_test_config(root.path(), "smoke", "smoke", "orangepi-5-plus");
        let legacy = root
            .path()
            .join("test-suit/starryos/smoke/.build-aarch64-unknown-none-softfloat.toml");
        fs::write(&legacy, "").unwrap();

        let err = discover_board_test_groups(root.path(), None, None)
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

        let groups = discover_board_test_groups(root.path(), None, None).unwrap();

        assert_eq!(groups[0].build_config_path, build);
    }

    #[test]
    fn qemu_summary_lists_passed_and_failed_cases() {
        let report = StarryQemuRunReport {
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

        assert!(summary.contains("starry qemu test summary:"));
        assert!(summary.contains("  PASS smoke (0.50s)"));
        assert!(summary.contains("  FAIL usb (2.00s)"));
        assert!(summary.contains("result: 1/2 case(s) passed"));
        assert!(summary.contains("total: 3.00s"));
    }
}
