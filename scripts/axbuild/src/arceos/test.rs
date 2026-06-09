use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};

use anyhow::{Context, bail};
use clap::{Args, Subcommand};
use ostool::{build::config::Cargo, run::qemu::QemuConfig};
use regex::Regex;

use super::{ArceOS, build, cbuild, ensure_qemu_runtime_assets};
use crate::{
    context::{BuildCliArgs, ResolvedBuildRequest, SnapshotPersistence},
    test::{
        case::TestQemuCase, host_http::HostHttpServerGuard, qemu as qemu_test, suite as test_suite,
    },
};

const ARCEOS_RUST_TEST_GROUP: &str = "rust";
const ARCEOS_C_TEST_GROUP: &str = "c";
const ARCEOS_TEST_SUITE_OS: &str = "arceos";
const ARCEOS_RUST_TEST_PACKAGE: &str = "arceos-test-suit";
const ARCEOS_RUST_TEST_BUILD_GROUP: &str = "arceos-test-suit";
const ARCEOS_C_TEST_BUILD_GROUP: &str = "arceos-c-test-suit";

const ARCEOS_RUST_ALL_FEATURE: &str = "all";
const ARCEOS_C_ALL_FEATURE: &str = "all";
const ARCEOS_RUST_DEBUG_BACKTRACE_FEATURE: &str = "debug-backtrace";
const ARCEOS_RUST_DEBUG_PANIC_PATH_FEATURE: &str = "debug-panic-path";
const ARCEOS_RUST_EXCEPTION_PAGE_FAULT_FEATURE: &str = "exception-page-fault";
const ARCEOS_RUST_LOCKDEP_DETECT_FEATURE: &str = "lockdep-detect";
const ARCEOS_RUST_STACK_GUARD_PAGE_FEATURE: &str = "task-stack-guard-page";

const ARCEOS_RUST_QEMU_FEATURES: &[&str] = &[
    ARCEOS_RUST_ALL_FEATURE,
    ARCEOS_RUST_DEBUG_BACKTRACE_FEATURE,
    ARCEOS_RUST_DEBUG_PANIC_PATH_FEATURE,
    "display-basic",
    "exception-breakpoint",
    ARCEOS_RUST_EXCEPTION_PAGE_FAULT_FEATURE,
    "fs-basic",
    "lockdep-baseline",
    ARCEOS_RUST_LOCKDEP_DETECT_FEATURE,
    "memtest",
    "net-loopback",
    "sched-cfs",
    "sched-rr",
    "task-affinity",
    "task-ipi",
    "task-irq",
    "task-parallel",
    "task-priority",
    "task-sleep",
    ARCEOS_RUST_STACK_GUARD_PAGE_FEATURE,
    "task-tls",
    "task-wait-queue",
    "task-wait-queue-remote-wake",
    "task-yield",
];

const ARCEOS_C_QEMU_FEATURES: &[&str] = &[
    ARCEOS_C_ALL_FEATURE,
    "mem",
    "pthread-basic",
    "pthread-parallel",
    "pthread-sleep",
    "pipe",
    "epoll",
    "net-http",
];
const ARCEOS_C_QEMU_LISTED_CASES: &[&str] = &[
    "mem",
    "pthread-basic",
    "pthread-parallel",
    "pthread-sleep",
    "pipe",
    "epoll",
    "net-http",
];

#[derive(Args)]
pub struct ArgsTest {
    #[command(subcommand)]
    pub command: TestCommand,
}

#[derive(Subcommand)]
pub enum TestCommand {
    /// Run ArceOS QEMU test suites (Rust + C by default)
    Qemu(ArgsTestQemu),
}

#[derive(Args, Debug, Clone)]
pub struct ArgsTestQemu {
    #[arg(
        long,
        value_name = "ARCH",
        required_unless_present_any = ["target", "list"],
        help = "ArceOS architecture to test"
    )]
    pub arch: Option<String>,
    #[arg(
        short = 't',
        long,
        value_name = "TARGET",
        required_unless_present_any = ["arch", "list"],
        help = "ArceOS target triple to test"
    )]
    pub target: Option<String>,
    #[arg(
        short = 'g',
        long = "test-group",
        value_name = "GROUP",
        help = "Run ArceOS QEMU test cases from one test group (rust or c)"
    )]
    pub test_group: Option<String>,
    #[arg(
        short = 'c',
        long = "test-case",
        value_name = "CASE",
        help = "Run only one ArceOS QEMU test case"
    )]
    pub test_case: Option<String>,
    #[arg(short = 'l', long, help = "List discovered ArceOS QEMU test cases")]
    pub list: bool,
    /// Removed: Rust tests are selected with `--test-case`.
    #[arg(
        short,
        long,
        value_name = "PACKAGE",
        conflicts_with = "only_c",
        hide = true
    )]
    pub package: Vec<String>,
    /// Only run Rust tests; prefer `--test-group rust`
    #[arg(long, conflicts_with = "only_c", hide = true)]
    pub only_rust: bool,
    /// Only run C tests; prefer `--test-group c`
    #[arg(long, conflicts_with = "only_rust", hide = true)]
    pub only_c: bool,
    /// Skip host `backtrace symbolize` after each ArceOS **rust** QEMU case.
    #[arg(long = "no-symbolize", help_heading = "Backtrace")]
    pub no_symbolize: bool,
    /// Keep the QEMU backtrace capture log after successful host symbolize (default: delete).
    #[arg(long = "keep-qemu-log", help_heading = "Backtrace")]
    pub keep_qemu_log: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum QemuTestFlow {
    Rust,
    C,
    Generic(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ArceosRustQemuCase {
    case: TestQemuCase,
    build_group: String,
    build_config_path: PathBuf,
    package: String,
    feature: Option<String>,
}

#[derive(Debug, Clone)]
struct PreparedArceosRustQemuCase {
    case: ArceosRustQemuCase,
    request: ResolvedBuildRequest,
    cargo: Cargo,
    qemu: QemuConfig,
    host_symbolize_success_regex: Vec<String>,
}

struct ArceosQemuBuildGroup<'a> {
    build_group: &'a str,
    build_config_path: &'a Path,
    package: &'a str,
    feature: Option<&'a str>,
    request: ResolvedBuildRequest,
    cargo: Cargo,
    cases: Vec<&'a PreparedArceosRustQemuCase>,
}

struct GenericQemuRunOptions<'a> {
    selected_case: Option<&'a str>,
    symbolize_after: bool,
    keep_qemu_log: bool,
    allow_empty: bool,
}

impl qemu_test::BuildConfigRef for PreparedArceosRustQemuCase {
    fn build_group(&self) -> &str {
        &self.case.build_group
    }

    fn build_config_path(&self) -> &Path {
        &self.case.build_config_path
    }
}

/// A discovered C test under `test-suit/arceos/c/`.
struct CTestDef {
    name: String,
    build_group: String,
    build_config_path: PathBuf,
    qemu_config_path: PathBuf,
}

impl qemu_test::BuildConfigRef for CTestDef {
    fn build_group(&self) -> &str {
        &self.build_group
    }

    fn build_config_path(&self) -> &Path {
        &self.build_config_path
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CTestArtifactPaths {
    target_dir: PathBuf,
    out_dir: PathBuf,
}

pub(super) async fn test(arceos: &mut ArceOS, args: ArgsTest) -> anyhow::Result<()> {
    match args.command {
        TestCommand::Qemu(args) => test_qemu(arceos, args).await,
    }
}

async fn test_qemu(arceos: &mut ArceOS, args: ArgsTestQemu) -> anyhow::Result<()> {
    reject_removed_rust_package_filter(&args)?;

    if args.list && args.arch.is_none() && args.target.is_none() && args.test_group.is_none() {
        let groups = all_qemu_case_groups(arceos, args.test_case.as_deref())?;
        if groups.is_empty() {
            bail!(
                "no ArceOS qemu test cases found under {}",
                arceos
                    .app
                    .workspace_root()
                    .join("test-suit")
                    .join(ARCEOS_TEST_SUITE_OS)
                    .display()
            );
        }
        println!("{}", qemu_test::render_qemu_case_forest("arceos", groups));
        return Ok(());
    }

    if args.list && args.arch.is_none() && args.target.is_none() {
        let groups = selected_qemu_test_groups(arceos.app.workspace_root(), &args)?;
        let allow_rust_case_miss = args.test_group.is_none() && !args.only_rust;
        let mut trees = Vec::new();
        for group in groups {
            match group {
                QemuTestFlow::Rust => trees.extend(list_rust_qemu_cases(
                    arceos,
                    None,
                    args.test_case.as_deref(),
                    allow_rust_case_miss,
                )?),
                QemuTestFlow::C => {
                    trees.extend(list_c_qemu_cases(arceos, None, args.test_case.as_deref())?)
                }
                QemuTestFlow::Generic(ref group) => trees.extend(list_generic_qemu_cases(
                    arceos,
                    None,
                    group,
                    args.test_case.as_deref(),
                )?),
            }
        }
        if trees.is_empty() {
            bail!("no ArceOS qemu test cases found");
        }
        println!("{}", trees.join("\n"));
        return Ok(());
    }

    let selected_case = args.test_case.as_deref();
    let (arch, target) = qemu_test::parse_test_target(
        &args.arch,
        &args.target,
        "arceos qemu tests",
        &crate::context::supported_arches(),
        &crate::context::supported_targets(),
        crate::context::resolve_arceos_arch_and_target,
    )?;
    let groups = selected_qemu_test_groups(arceos.app.workspace_root(), &args)?;
    let allow_rust_case_miss = args.test_group.is_none() && !args.only_rust;
    if args.list {
        let mut trees = Vec::new();
        for group in groups {
            match group {
                QemuTestFlow::Rust => trees.extend(list_rust_qemu_cases(
                    arceos,
                    Some((&arch, &target)),
                    selected_case,
                    allow_rust_case_miss,
                )?),
                QemuTestFlow::C => trees.extend(list_c_qemu_cases(
                    arceos,
                    Some((&arch, &target)),
                    args.test_case.as_deref(),
                )?),
                QemuTestFlow::Generic(ref group) => trees.extend(list_generic_qemu_cases(
                    arceos,
                    Some((&arch, &target)),
                    group,
                    selected_case,
                )?),
            }
        }
        if trees.is_empty() {
            bail!("no ArceOS qemu test cases found");
        }
        println!("{}", trees.join("\n"));
        return Ok(());
    }

    let symbolize_after = !args.no_symbolize;
    let keep_qemu_log = args.keep_qemu_log || crate::backtrace::keep_qemu_log_from_env();
    for flow in groups {
        match flow {
            QemuTestFlow::Rust => {
                test_rust_qemu(
                    arceos,
                    &arch,
                    &target,
                    selected_case,
                    allow_rust_case_miss,
                    symbolize_after,
                    keep_qemu_log,
                )
                .await?
            }
            QemuTestFlow::C => test_c_qemu(arceos, &target, args.test_case.as_deref()).await?,
            QemuTestFlow::Generic(ref group) => {
                test_generic_qemu(
                    arceos,
                    &arch,
                    &target,
                    group,
                    GenericQemuRunOptions {
                        selected_case,
                        symbolize_after,
                        keep_qemu_log,
                        allow_empty: args.test_group.is_none(),
                    },
                )
                .await?
            }
        }
    }
    Ok(())
}

async fn test_rust_qemu(
    arceos: &mut ArceOS,
    arch: &str,
    target: &str,
    selected_case: Option<&str>,
    allow_missing_selected_case: bool,
    symbolize_after: bool,
    keep_qemu_log: bool,
) -> anyhow::Result<()> {
    let cases = discover_rust_qemu_cases(
        arceos,
        arch,
        target,
        selected_case,
        allow_missing_selected_case,
    )?;
    if cases.is_empty() {
        println!(
            "skipping arceos rust qemu tests for arch: {arch} (target: {target}, no matching \
             feature)"
        );
        return Ok(());
    }
    println!(
        "running arceos rust qemu tests for arch: {} (target: {}, cases: {})",
        arch,
        target,
        cases.len()
    );

    let prepared = prepare_rust_qemu_cases(arceos, target, cases).await?;
    let total = prepared.len();

    let build_groups = group_arceos_qemu_cases_by_build_identity(&prepared);
    let suite_started = Instant::now();
    let mut summary = qemu_test::QemuTestSummary::default();
    let mut completed = 0;
    for build_group in &build_groups {
        arceos
            .app
            .build(
                build_group.cargo.clone(),
                build_group.request.build_info_path.clone(),
            )
            .await
            .with_context(|| {
                let feature = build_group
                    .feature
                    .map(|feature| format!(" with feature `{feature}`"))
                    .unwrap_or_default();
                format!(
                    "failed to build ArceOS rust qemu test artifact for package `{}`{feature} in \
                     build group `{}` ({})",
                    build_group.package,
                    build_group.build_group,
                    build_group.build_config_path.display()
                )
            })?;

        for case in &build_group.cases {
            completed += 1;
            let case_name = &case.case.case.name;
            println!("[{completed}/{total}] arceos rust qemu {case_name}");
            let case_started = Instant::now();
            let result = run_rust_qemu_case(arceos, case, symbolize_after, keep_qemu_log)
                .await
                .with_context(|| format!("arceos rust qemu test failed for case `{case_name}`"));
            let duration = case_started.elapsed();
            match result {
                Ok(()) => {
                    println!("ok: {case_name} ({duration:.2?})");
                    summary.pass_with_detail(case_name, format!("{duration:.2?}"));
                }
                Err(err) => {
                    eprintln!("failed: {}: {:#}", case_name, err);
                    summary.fail_with_detail(case_name, format!("{duration:.2?}"));
                }
            }
        }
    }

    let total_duration = format!("{:.2?}", suite_started.elapsed());
    summary.finish_with_total_detail("arceos rust", "case", Some(total_duration.as_str()))
}

async fn test_c_qemu(
    arceos: &mut ArceOS,
    target: &str,
    selected_case: Option<&str>,
) -> anyhow::Result<()> {
    test_c_qemu_axbuild(arceos, target, selected_case).await
}

async fn test_generic_qemu(
    arceos: &mut ArceOS,
    arch: &str,
    target: &str,
    group: &str,
    options: GenericQemuRunOptions<'_>,
) -> anyhow::Result<()> {
    let dir = arceos_test_group_dir(arceos.app.workspace_root(), group);
    let cases = if options.allow_empty && options.selected_case.is_none() {
        discover_qemu_cases_in_dir_allow_empty(&dir, arch, target, options.selected_case, group)?
    } else {
        discover_qemu_cases_in_dir(&dir, arch, target, options.selected_case, group)?
    };
    if cases.is_empty() {
        println!(
            "skipping arceos {group} qemu tests for arch: {arch} (target: {target}, no cases)"
        );
        return Ok(());
    }
    let group_label = format!("arceos {group}");
    println!(
        "running {group_label} qemu tests for arch: {arch} (target: {target}, cases: {})",
        cases.len()
    );
    let prepared = prepare_rust_qemu_cases(arceos, target, cases).await?;
    let total = prepared.len();

    run_generic_qemu_by_build_group(
        arceos,
        group,
        &group_label,
        &prepared,
        total,
        options.symbolize_after,
        options.keep_qemu_log,
    )
    .await
}

async fn run_generic_qemu_by_build_group(
    arceos: &mut ArceOS,
    group: &str,
    group_label: &str,
    prepared: &[PreparedArceosRustQemuCase],
    total: usize,
    symbolize_after: bool,
    keep_qemu_log: bool,
) -> anyhow::Result<()> {
    let build_groups = group_arceos_qemu_cases_by_build_identity(prepared);
    let suite_started = Instant::now();
    let mut summary = qemu_test::QemuTestSummary::default();
    let mut completed = 0;
    for build_group in &build_groups {
        arceos
            .app
            .build(
                build_group.cargo.clone(),
                build_group.request.build_info_path.clone(),
            )
            .await
            .with_context(|| {
                let feature = build_group
                    .feature
                    .map(|feature| format!(" with feature `{feature}`"))
                    .unwrap_or_default();
                format!(
                    "failed to build ArceOS {group} qemu test artifact for package `{}`{feature} \
                     in build group `{}` ({})",
                    build_group.package,
                    build_group.build_group,
                    build_group.build_config_path.display()
                )
            })?;

        for case in &build_group.cases {
            completed += 1;
            let case_name = &case.case.case.name;
            println!("[{completed}/{total}] {group_label} qemu {case_name}");
            let case_started = Instant::now();
            let result = run_rust_qemu_case(arceos, case, symbolize_after, keep_qemu_log)
                .await
                .with_context(|| format!("{group_label} qemu test failed for case `{case_name}`"));
            let duration = case_started.elapsed();
            match result {
                Ok(()) => {
                    println!("ok: {case_name} ({duration:.2?})");
                    summary.pass_with_detail(case_name, format!("{duration:.2?}"));
                }
                Err(err) => {
                    eprintln!("failed: {case_name}: {err:#}");
                    summary.fail_with_detail(case_name, format!("{duration:.2?}"));
                }
            }
        }
    }
    let total_duration = format!("{:.2?}", suite_started.elapsed());
    summary.finish_with_total_detail(group_label, "case", Some(total_duration.as_str()))
}

fn group_arceos_qemu_cases_by_build_identity(
    cases: &[PreparedArceosRustQemuCase],
) -> Vec<ArceosQemuBuildGroup<'_>> {
    let mut groups = Vec::<ArceosQemuBuildGroup<'_>>::new();
    for case in cases {
        if let Some(group) = groups.iter_mut().find(|group| {
            group.build_config_path == case.case.build_config_path
                && group.package == case.case.package
                && group.feature == case.case.feature.as_deref()
        }) {
            group.cases.push(case);
            continue;
        }

        groups.push(ArceosQemuBuildGroup {
            build_group: &case.case.build_group,
            build_config_path: &case.case.build_config_path,
            package: &case.case.package,
            feature: case.case.feature.as_deref(),
            request: case.request.clone(),
            cargo: case.cargo.clone(),
            cases: vec![case],
        });
    }
    groups
}

fn test_build_args(package: &str, target: &str, config: &Path) -> BuildCliArgs {
    BuildCliArgs {
        config: Some(config.to_path_buf()),
        package: Some(package.to_string()),
        arch: None,
        target: Some(target.to_string()),
        plat_dyn: None,
        smp: None,
        debug: false,
    }
}

fn arceos_rust_test_dir(arceos: &ArceOS) -> PathBuf {
    arceos_test_group_dir(arceos.app.workspace_root(), ARCEOS_RUST_TEST_GROUP)
}

fn arceos_c_test_dir(arceos: &ArceOS) -> PathBuf {
    arceos_test_group_dir(arceos.app.workspace_root(), ARCEOS_C_TEST_GROUP)
}

fn arceos_test_group_dir(workspace_root: &Path, group: &str) -> PathBuf {
    test_suite::group_dir(workspace_root, ARCEOS_TEST_SUITE_OS, group)
}

fn discover_rust_qemu_cases(
    arceos: &ArceOS,
    arch: &str,
    target: &str,
    selected_case: Option<&str>,
    allow_missing_selected_case: bool,
) -> anyhow::Result<Vec<ArceosRustQemuCase>> {
    let root = arceos_rust_test_dir(arceos);
    rust_qemu_features_for_run(selected_case, allow_missing_selected_case)?
        .into_iter()
        .map(|feature| load_arceos_test_suit_qemu_case(&root, arch, target, feature))
        .collect()
}

fn discover_qemu_cases_in_dir(
    dir: &Path,
    arch: &str,
    target: &str,
    selected_case: Option<&str>,
    group: &str,
) -> anyhow::Result<Vec<ArceosRustQemuCase>> {
    qemu_test::discover_qemu_cases(dir, arch, target, selected_case, "ArceOS", group)?
        .into_iter()
        .map(load_rust_qemu_case)
        .collect::<anyhow::Result<Vec<_>>>()
}

fn discover_qemu_cases_in_dir_allow_empty(
    dir: &Path,
    arch: &str,
    target: &str,
    selected_case: Option<&str>,
    group: &str,
) -> anyhow::Result<Vec<ArceosRustQemuCase>> {
    qemu_test::discover_qemu_cases_allow_empty(dir, arch, target, selected_case, "ArceOS", group)?
        .into_iter()
        .map(load_rust_qemu_case)
        .collect::<anyhow::Result<Vec<_>>>()
}

fn load_rust_qemu_case(case: qemu_test::DiscoveredQemuCase) -> anyhow::Result<ArceosRustQemuCase> {
    let package = read_manifest_package_name(&case.case_dir.join("Cargo.toml"))?;
    let host_http_server = qemu_test::load_qemu_case_host_http_server(&case.qemu_config_path)?;
    Ok(ArceosRustQemuCase {
        case: TestQemuCase {
            name: case.name,
            display_name: case.display_name,
            case_dir: case.case_dir,
            qemu_config_path: case.qemu_config_path,
            test_commands: Vec::new(),
            host_symbolize_success_regex: Vec::new(),
            host_http_server,
            subcases: Vec::new(),
        },
        build_group: case.build_group,
        build_config_path: case.build_config_path,
        package,
        feature: None,
    })
}

fn load_arceos_test_suit_qemu_case(
    root: &Path,
    arch: &str,
    target: &str,
    feature: &str,
) -> anyhow::Result<ArceosRustQemuCase> {
    let build_config_path = arceos_test_suit_build_config_path(root, target)?;
    let qemu_config_path = arceos_test_suit_qemu_config_path(root, arch)?;
    let host_http_server = qemu_test::load_qemu_case_host_http_server(&qemu_config_path)?;
    Ok(ArceosRustQemuCase {
        case: TestQemuCase {
            name: feature.to_string(),
            display_name: feature.to_string(),
            case_dir: root.to_path_buf(),
            qemu_config_path,
            test_commands: Vec::new(),
            host_symbolize_success_regex: Vec::new(),
            host_http_server,
            subcases: Vec::new(),
        },
        build_group: ARCEOS_RUST_TEST_BUILD_GROUP.to_string(),
        build_config_path,
        package: ARCEOS_RUST_TEST_PACKAGE.to_string(),
        feature: Some(feature.to_string()),
    })
}

fn arceos_test_suit_build_config_path(root: &Path, target: &str) -> anyhow::Result<PathBuf> {
    let path = root.join(format!("build-{target}.toml"));
    if path.is_file() {
        return Ok(path);
    }
    bail!("ArceOS rust test suite must provide {}", path.display())
}

fn arceos_test_suit_qemu_config_path(root: &Path, arch: &str) -> anyhow::Result<PathBuf> {
    let path = root.join(qemu_test::qemu_config_name(arch));
    if path.is_file() {
        return Ok(path);
    }
    bail!("ArceOS rust test suite must provide {}", path.display())
}

async fn prepare_rust_qemu_cases(
    arceos: &mut ArceOS,
    target: &str,
    cases: Vec<ArceosRustQemuCase>,
) -> anyhow::Result<Vec<PreparedArceosRustQemuCase>> {
    let mut prepared = Vec::with_capacity(cases.len());
    for case in cases {
        let request = arceos.prepare_request(
            test_build_args(&case.package, target, &case.build_config_path),
            Some(case.case.qemu_config_path.clone()),
            None,
            SnapshotPersistence::Discard,
        )?;
        let mut cargo = build::load_cargo_config(&request)?;
        if let Some(feature) = case.feature.as_deref() {
            add_cargo_feature(&mut cargo, feature);
        }
        let mut qemu = arceos
            .load_qemu_config(&request, &cargo)
            .await?
            .with_context(|| {
                format!(
                    "failed to load ArceOS qemu config for case `{}`",
                    case.case.display_name
                )
            })?;
        let build_info: build::ArceosBuildInfo =
            crate::build::load_build_info(&request.build_info_path)?;
        qemu_test::apply_smp_qemu_arg(
            &mut qemu,
            request.smp.or(build_info.max_cpu_num).or(Some(1)),
        );
        apply_rust_qemu_feature_overrides(&mut cargo, &mut qemu, case.feature.as_deref());
        qemu_test::apply_timeout_scale(&mut qemu);
        ensure_qemu_runtime_assets(arceos.app.workspace_root(), &qemu)?;
        prepared.push(PreparedArceosRustQemuCase {
            host_symbolize_success_regex: rust_qemu_host_symbolize_success_regex(
                case.feature.as_deref(),
            ),
            case,
            request,
            cargo,
            qemu,
        });
    }
    Ok(prepared)
}

fn rust_qemu_host_symbolize_success_regex(feature: Option<&str>) -> Vec<String> {
    match feature {
        Some(ARCEOS_RUST_DEBUG_BACKTRACE_FEATURE) => vec![
            r"(?s)BACKTRACE_BLOCK\s+\d+\s+kind=arceos-test-suit-raw-normal\b.*\bdebug::backtrace::nested_c\b.*\bdebug::backtrace::nested_b\b.*\bdebug::backtrace::nested_a\b"
                .to_string(),
            r"(?s)BACKTRACE_BLOCK\s+\d+\s+kind=arceos-test-suit-raw-badfp\b.*BT\s+0\s+ip=0x[0-9a-fA-F]+"
                .to_string(),
        ],
        _ => Vec::new(),
    }
}

fn apply_rust_qemu_feature_overrides(
    cargo: &mut Cargo,
    qemu: &mut QemuConfig,
    feature: Option<&str>,
) {
    match feature {
        Some(ARCEOS_RUST_DEBUG_PANIC_PATH_FEATURE) => {
            qemu.success_regex = vec![r"BACKTRACE_BEGIN\b.*\bkind=panic\b".to_string()];
            qemu.fail_regex = vec!["ARCEOS_TEST_FAIL".to_string()];
            qemu.timeout = Some(qemu.timeout.unwrap_or(30).min(30));
        }
        Some(ARCEOS_RUST_EXCEPTION_PAGE_FAULT_FEATURE) => {
            qemu.success_regex = vec!["Page fault test OK!".to_string()];
            qemu.fail_regex = vec![
                r"(?i)\bpanic(?:ked)?\b".to_string(),
                "page fault handler did not stop the system".to_string(),
            ];
            qemu.timeout = Some(qemu.timeout.unwrap_or(30).min(30));
        }
        Some(ARCEOS_RUST_LOCKDEP_DETECT_FEATURE) => {
            qemu.success_regex = vec!["lockdep: lock order inversion detected".to_string()];
            qemu.fail_regex =
                vec!["lockdep did not report an expected lock order inversion".to_string()];
            qemu.timeout = Some(qemu.timeout.unwrap_or(30).min(30));
        }
        Some(ARCEOS_RUST_STACK_GUARD_PAGE_FEATURE) => {
            qemu.success_regex =
                vec!["task stack guard page hit for .*stack-guard-page-overflow".to_string()];
            qemu.fail_regex = vec!["stack guard page was not hit".to_string()];
            qemu.timeout = Some(qemu.timeout.unwrap_or(30).min(30));
        }
        Some("task-wait-queue-remote-wake")
            if cargo.target == "riscv64gc-unknown-none-elf"
                && !qemu.args.iter().any(|arg| arg == "-accel") =>
        {
            qemu.args.push("-accel".to_string());
            qemu.args.push("tcg,thread=single".to_string());
        }
        _ => {}
    }
}

fn add_cargo_feature(cargo: &mut Cargo, feature: &str) {
    if !cargo.features.iter().any(|existing| existing == feature) {
        cargo.features.push(feature.to_string());
        cargo.features.sort();
    }
}

async fn run_rust_qemu_case(
    arceos: &mut ArceOS,
    case: &PreparedArceosRustQemuCase,
    symbolize_after: bool,
    keep_qemu_log: bool,
) -> anyhow::Result<()> {
    let workspace = arceos.app.workspace_root().to_path_buf();
    let case_name = &case.case.case.name;
    let target = &case.request.target;
    let package = &case.case.package;
    let debug = case.request.debug;

    let auto_symbolize = symbolize_after
        && crate::build::build_info_enables_backtrace_path(&case.case.build_config_path);
    if !case.host_symbolize_success_regex.is_empty() && !auto_symbolize {
        bail!(
            "ArceOS rust qemu case `{case_name}` requires host symbolize assertions; do not use \
             --no-symbolize and keep BACKTRACE/DWARF enabled in the build config"
        );
    }

    let elf = crate::backtrace::std_test_elf_path(&workspace, target, package, debug);
    let stream_session = if auto_symbolize {
        crate::backtrace::BacktraceSymbolizeSession::try_new(&elf, case_name)
    } else {
        None
    };

    let capture_backtrace = if auto_symbolize {
        let dir = crate::context::axbuild_tmp_dir(&workspace).join("qemu-logs");
        fs::create_dir_all(&dir)?;
        Some(crate::backtrace::BacktraceQemuCapture {
            log_path: dir.join(format!("{case_name}-{target}.log")),
            stream_symbolize: stream_session.clone(),
            suppress_terminal_raw_blocks: true,
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

    let _host_http_server = case
        .case
        .case
        .host_http_server
        .as_ref()
        .map(|config| HostHttpServerGuard::start(config, case_name))
        .transpose()?;

    arceos
        .app
        .run_qemu(&case.cargo, case.qemu.clone(), capture_backtrace)
        .await
        .with_context(|| format!("failed to run ArceOS rust qemu test case `{case_name}`"))?;

    if auto_symbolize && let Some(path) = log_path {
        let blocks_snapshot = memory_blocks.and_then(|arc| arc.lock().ok().map(|b| b.clone()));
        let symbolized_output = if !case.host_symbolize_success_regex.is_empty() {
            match blocks_snapshot.as_deref() {
                Some(blocks) => {
                    crate::backtrace::symbolize_captured_blocks_to_string(&elf, case_name, blocks)?
                }
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
            ensure_arceos_host_symbolize_output_matches(
                case_name,
                outcome,
                symbolized_output.as_deref(),
                &case.host_symbolize_success_regex,
            )?;
        }
    }

    Ok(())
}

fn ensure_arceos_host_symbolize_output_matches(
    case_name: &str,
    outcome: crate::backtrace::SymbolizeAfterQemuOutcome,
    output: Option<&str>,
    regexes: &[String],
) -> anyhow::Result<()> {
    if outcome != crate::backtrace::SymbolizeAfterQemuOutcome::Symbolized {
        bail!("host backtrace symbolize did not run for ArceOS rust qemu case `{case_name}`");
    }
    let output =
        output.ok_or_else(|| anyhow::anyhow!("host backtrace symbolize produced no output"))?;
    for pattern in regexes {
        let regex = Regex::new(pattern)
            .with_context(|| format!("invalid host_symbolize_success_regex `{pattern}`"))?;
        if !regex.is_match(output) {
            bail!(
                "host backtrace symbolize output for ArceOS rust qemu case `{case_name}` did not \
                 match `{pattern}`"
            );
        }
    }
    Ok(())
}

fn list_rust_qemu_cases(
    arceos: &ArceOS,
    target: Option<(&str, &str)>,
    selected_case: Option<&str>,
    allow_missing_selected_case: bool,
) -> anyhow::Result<Option<String>> {
    let cases = rust_qemu_case_names(arceos, target, selected_case, allow_missing_selected_case)?;
    if cases.is_empty() {
        return Ok(None);
    }
    Ok(Some(qemu_test::render_case_tree(
        ARCEOS_RUST_TEST_GROUP,
        cases,
    )))
}

fn list_c_qemu_cases(
    arceos: &ArceOS,
    target: Option<(&str, &str)>,
    selected_case: Option<&str>,
) -> anyhow::Result<Option<String>> {
    let cases = c_qemu_case_names(arceos, target, selected_case)?;
    if cases.is_empty() {
        return Ok(None);
    }
    Ok(Some(qemu_test::render_case_tree(
        ARCEOS_C_TEST_GROUP,
        cases,
    )))
}

fn list_generic_qemu_cases(
    arceos: &ArceOS,
    target: Option<(&str, &str)>,
    group: &str,
    selected_case: Option<&str>,
) -> anyhow::Result<Option<String>> {
    let dir = arceos_test_group_dir(arceos.app.workspace_root(), group);
    let cases: Vec<String> = match target {
        Some((arch, target)) => {
            discover_qemu_cases_in_dir(&dir, arch, target, selected_case, group)?
                .into_iter()
                .map(|case| case.case.name)
                .collect()
        }
        None => qemu_test::discover_all_qemu_cases(&dir, selected_case, "ArceOS", group)
            .map_err(anyhow::Error::new)?,
    };
    if cases.is_empty() {
        return Ok(None);
    }
    Ok(Some(qemu_test::render_case_tree(group, cases)))
}

fn all_qemu_case_groups(
    arceos: &ArceOS,
    selected_case: Option<&str>,
) -> anyhow::Result<Vec<(String, Vec<qemu_test::ListedQemuCase>)>> {
    let mut groups = Vec::new();
    for group in
        test_suite::discover_group_names(arceos.app.workspace_root(), ARCEOS_TEST_SUITE_OS)?
    {
        let cases: Option<Vec<qemu_test::ListedQemuCase>> = match group.as_str() {
            ARCEOS_RUST_TEST_GROUP => rust_qemu_listed_cases(arceos, selected_case)
                .ok()
                .filter(|cases| !cases.is_empty()),
            ARCEOS_C_TEST_GROUP => c_qemu_listed_cases(arceos, selected_case)
                .ok()
                .filter(|v| !v.is_empty()),
            _ => {
                let dir = arceos_test_group_dir(arceos.app.workspace_root(), &group);
                match qemu_test::discover_all_qemu_cases_with_archs(
                    &dir,
                    selected_case,
                    "ArceOS",
                    &group,
                ) {
                    Ok(cases) if !cases.is_empty() => Some(cases),
                    Ok(_) => None,
                    Err(err) if qemu_list_error_is_ignorable(err.kind()) => None,
                    Err(err) => return Err(anyhow::Error::new(err)),
                }
            }
        };
        if let Some(cases) = cases {
            groups.push((group, cases));
        }
    }
    if groups.is_empty()
        && let Some(case) = selected_case
    {
        bail!("unknown ArceOS qemu test case `{case}`");
    }
    Ok(groups)
}

fn qemu_list_error_is_ignorable(kind: qemu_test::ListQemuCasesErrorKind) -> bool {
    matches!(
        kind,
        qemu_test::ListQemuCasesErrorKind::EmptyGroup
            | qemu_test::ListQemuCasesErrorKind::UnknownSelectedCase
    )
}

fn rust_qemu_listed_cases(
    arceos: &ArceOS,
    selected_case: Option<&str>,
) -> anyhow::Result<Vec<qemu_test::ListedQemuCase>> {
    let root = arceos_rust_test_dir(arceos);
    let archs = arceos_test_suit_qemu_archs(&root)?;
    if archs.is_empty() {
        bail!("no ArceOS rust qemu configs found under {}", root.display());
    }
    Ok(rust_qemu_features_for_list(selected_case, false)?
        .into_iter()
        .map(|feature| qemu_test::ListedQemuCase {
            name: feature.to_string(),
            archs: archs.clone(),
        })
        .collect())
}

fn rust_qemu_case_names(
    arceos: &ArceOS,
    target: Option<(&str, &str)>,
    selected_case: Option<&str>,
    allow_missing_selected_case: bool,
) -> anyhow::Result<Vec<String>> {
    match target {
        Some((arch, target)) => {
            let root = arceos_rust_test_dir(arceos);
            arceos_test_suit_build_config_path(&root, target)?;
            arceos_test_suit_qemu_config_path(&root, arch)?;
            Ok(
                rust_qemu_features_for_list(selected_case, allow_missing_selected_case)?
                    .into_iter()
                    .map(str::to_string)
                    .collect(),
            )
        }
        None => Ok(
            rust_qemu_features_for_list(selected_case, allow_missing_selected_case)?
                .into_iter()
                .map(str::to_string)
                .collect(),
        ),
    }
}

fn arceos_test_suit_qemu_archs(root: &Path) -> anyhow::Result<Vec<String>> {
    let mut archs = Vec::new();
    for entry in fs::read_dir(root).with_context(|| format!("failed to read {}", root.display()))? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() || path.extension().is_none_or(|ext| ext != "toml") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        if let Some(arch) = stem.strip_prefix("qemu-")
            && !arch.starts_with("base-")
        {
            archs.push(arch.to_string());
        }
    }
    archs.sort();
    Ok(archs)
}

fn rust_qemu_features_for_run(
    selected_case: Option<&str>,
    allow_missing_selected_case: bool,
) -> anyhow::Result<Vec<&'static str>> {
    match selected_case {
        Some(_) => rust_qemu_features_for_list(selected_case, allow_missing_selected_case),
        None => Ok(vec![ARCEOS_RUST_ALL_FEATURE]),
    }
}

fn rust_qemu_features_for_list(
    selected_case: Option<&str>,
    allow_missing_selected_case: bool,
) -> anyhow::Result<Vec<&'static str>> {
    let Some(selected_case) = selected_case else {
        return Ok(ARCEOS_RUST_QEMU_FEATURES.to_vec());
    };

    let features = ARCEOS_RUST_QEMU_FEATURES
        .iter()
        .copied()
        .filter(|feature| *feature == selected_case)
        .collect::<Vec<_>>();
    if features.is_empty() {
        if allow_missing_selected_case {
            return Ok(Vec::new());
        }
        bail!("unknown ArceOS rust qemu test feature `{selected_case}`");
    }
    Ok(features)
}

fn c_qemu_features_for_run(selected_case: Option<&str>) -> anyhow::Result<Vec<&'static str>> {
    match selected_case {
        Some(_) => c_qemu_features_for_list(selected_case),
        None => Ok(vec![ARCEOS_C_ALL_FEATURE]),
    }
}

fn c_qemu_features_for_list(selected_case: Option<&str>) -> anyhow::Result<Vec<&'static str>> {
    let Some(selected_case) = selected_case else {
        return Ok(ARCEOS_C_QEMU_LISTED_CASES.to_vec());
    };

    let features = ARCEOS_C_QEMU_FEATURES
        .iter()
        .copied()
        .filter(|feature| *feature == selected_case)
        .collect::<Vec<_>>();
    if features.is_empty() {
        bail!("unknown ArceOS c qemu test feature `{selected_case}`");
    }
    Ok(features)
}

fn c_qemu_case_names(
    arceos: &ArceOS,
    target: Option<(&str, &str)>,
    selected_case: Option<&str>,
) -> anyhow::Result<Vec<String>> {
    if let Some((arch, target)) = target {
        let root = arceos_c_test_dir(arceos);
        arceos_c_test_suit_build_config_path(&root, target)?;
        arceos_c_test_suit_qemu_config_path(&root, arch)?;
    }

    Ok(c_qemu_features_for_list(selected_case)?
        .into_iter()
        .map(str::to_string)
        .collect())
}

fn c_qemu_listed_cases(
    arceos: &ArceOS,
    selected_case: Option<&str>,
) -> qemu_test::ListQemuCasesResult<Vec<qemu_test::ListedQemuCase>> {
    let root = arceos_c_test_dir(arceos);
    let archs = arceos_test_suit_qemu_archs(&root).map_err(qemu_test::ListQemuCasesError::from)?;
    if archs.is_empty() {
        return Ok(Vec::new());
    }
    let Ok(features) = c_qemu_features_for_list(selected_case) else {
        return Ok(Vec::new());
    };
    Ok(features
        .into_iter()
        .map(|feature| qemu_test::ListedQemuCase {
            name: feature.to_string(),
            archs: archs.clone(),
        })
        .collect())
}

fn reject_removed_rust_package_filter(args: &ArgsTestQemu) -> anyhow::Result<()> {
    if args.package.is_empty() {
        return Ok(());
    }
    bail!(
        "ArceOS rust qemu tests no longer support --package; use --test-case <case> to select a \
         feature-gated test, or omit it to run the `all` feature in one QEMU boot"
    )
}

fn read_manifest_package_name(path: &Path) -> anyhow::Result<String> {
    let contents =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let value: toml::Value =
        toml::from_str(&contents).with_context(|| format!("failed to parse {}", path.display()))?;
    value
        .get("package")
        .and_then(toml::Value::as_table)
        .and_then(|package| package.get("name"))
        .and_then(toml::Value::as_str)
        .map(str::to_string)
        .with_context(|| format!("missing package.name in {}", path.display()))
}

fn selected_qemu_test_groups(
    workspace_root: &Path,
    args: &ArgsTestQemu,
) -> anyhow::Result<Vec<QemuTestFlow>> {
    if args.only_c {
        return Ok(vec![QemuTestFlow::C]);
    }
    if args.only_rust {
        return Ok(vec![QemuTestFlow::Rust]);
    }

    match args.test_group.as_deref() {
        None => {
            let mut flows = vec![QemuTestFlow::Rust, QemuTestFlow::C];
            for group in test_suite::discover_group_names(workspace_root, ARCEOS_TEST_SUITE_OS)? {
                if group != ARCEOS_RUST_TEST_GROUP && group != ARCEOS_C_TEST_GROUP {
                    flows.push(QemuTestFlow::Generic(group));
                }
            }
            Ok(flows)
        }
        Some(ARCEOS_RUST_TEST_GROUP) => Ok(vec![QemuTestFlow::Rust]),
        Some(ARCEOS_C_TEST_GROUP) => Ok(vec![QemuTestFlow::C]),
        Some(group) => {
            let dir = test_suite::group_dir(workspace_root, ARCEOS_TEST_SUITE_OS, group);
            if dir.is_dir() {
                Ok(vec![QemuTestFlow::Generic(group.to_string())])
            } else {
                bail!(
                    "unsupported ArceOS qemu test group `{group}`; supported groups are: {}",
                    test_suite::supported_group_names(workspace_root, ARCEOS_TEST_SUITE_OS)
                        .unwrap_or_else(|_| {
                            format!("{ARCEOS_RUST_TEST_GROUP}, {ARCEOS_C_TEST_GROUP}")
                        })
                )
            }
        }
    }
}

fn load_arceos_c_test_suit_qemu_case(
    root: &Path,
    arch: &str,
    target: &str,
    feature: &str,
) -> anyhow::Result<CTestDef> {
    Ok(CTestDef {
        name: feature.to_string(),
        build_group: ARCEOS_C_TEST_BUILD_GROUP.to_string(),
        build_config_path: arceos_c_test_suit_build_config_path(root, target)?,
        qemu_config_path: arceos_c_test_suit_qemu_config_path(root, arch)?,
    })
}

fn arceos_c_test_suit_build_config_path(root: &Path, target: &str) -> anyhow::Result<PathBuf> {
    let path = root.join(format!("build-{target}.toml"));
    if path.is_file() {
        return Ok(path);
    }
    bail!("ArceOS C test suite must provide {}", path.display())
}

fn arceos_c_test_suit_qemu_config_path(root: &Path, arch: &str) -> anyhow::Result<PathBuf> {
    let path = root.join(qemu_test::qemu_config_name(arch));
    if path.is_file() {
        return Ok(path);
    }
    bail!("ArceOS C test suite must provide {}", path.display())
}

fn load_c_test_build_config(path: &Path) -> anyhow::Result<build::ArceosBuildConfig> {
    let config = build::load_arceos_build_config(path)
        .with_context(|| format!("failed to parse C build config {}", path.display()))?;
    if config.app_c.is_none() {
        bail!(
            "ArceOS C qemu test build config {} must set `app-c = \"c\"` or another C source \
             directory",
            path.display()
        );
    }
    Ok(config)
}

fn load_c_test_qemu_config(path: &Path) -> anyhow::Result<QemuConfig> {
    toml::from_str(&fs::read_to_string(path)?)
        .with_context(|| format!("failed to parse C qemu config {}", path.display()))
}

fn c_test_artifact_index(test: &CTestDef) -> usize {
    let stem = test
        .qemu_config_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("qemu");
    match stem.strip_prefix("qemu-") {
        Some("x86_64") => 0,
        Some("aarch64") => 1,
        Some("riscv64") => 2,
        Some("loongarch64") => 3,
        Some(_) | None => 0,
    }
}

fn c_test_display_suffix(path: &Path) -> String {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .and_then(|stem| stem.strip_prefix("qemu-"))
        .map(|arch| format!(" ({arch})"))
        .unwrap_or_default()
}

async fn test_c_qemu_axbuild(
    arceos: &mut ArceOS,
    target: &str,
    selected_case: Option<&str>,
) -> anyhow::Result<()> {
    let arch = crate::context::arch_for_target_checked(target)?;
    let c_test_root = arceos_c_test_dir(arceos);
    let c_tests = c_qemu_features_for_run(selected_case)?
        .into_iter()
        .map(|feature| load_arceos_c_test_suit_qemu_case(&c_test_root, arch, target, feature))
        .collect::<anyhow::Result<Vec<_>>>()?;
    if c_tests.is_empty() {
        println!("no C tests found in {}", c_test_root.display());
        return Ok(());
    }

    println!(
        "running arceos C qemu tests for {} test(s) on target: {} (arch: {})",
        c_tests.len(),
        target,
        arch
    );

    let mut summary = qemu_test::QemuTestSummary::default();
    let total = c_tests.len();
    let suite_started = Instant::now();
    for (index, c_test) in c_tests.into_iter().enumerate() {
        println!("[{}/{}] arceos c qemu {}", index + 1, total, c_test.name);
        let case_started = Instant::now();
        let result = build_and_run_c_test(arceos, target, arch, &c_test)
            .await
            .with_context(|| {
                format!(
                    "c test `{}` failed{}",
                    c_test.name,
                    c_test_display_suffix(&c_test.qemu_config_path)
                )
            });
        let duration = case_started.elapsed();
        if let Err(err) = result {
            eprintln!("failed: c/{}: {err:#}", c_test.name);
            summary.fail_with_detail(format!("c/{}", c_test.name), format!("{duration:.2?}"));
        } else {
            println!("ok: c/{} ({duration:.2?})", c_test.name);
            summary.pass_with_detail(format!("c/{}", c_test.name), format!("{duration:.2?}"));
        }
    }

    let total_duration = format!("{:.2?}", suite_started.elapsed());
    summary.finish_with_total_detail("arceos c", "test", Some(total_duration.as_str()))
}

async fn build_and_run_c_test(
    arceos: &mut ArceOS,
    target: &str,
    _arch: &str,
    test: &CTestDef,
) -> anyhow::Result<()> {
    let workspace_root = arceos.app.workspace_root().to_path_buf();
    let build_config = load_c_test_build_config(&test.build_config_path)?;
    let qemu_config = load_c_test_qemu_config(&test.qemu_config_path)?;
    let mode = build::load_arceos_build_mode(&test.build_config_path)?;
    let build::ArceosBuildMode::AppC { app_dir, app_name } = mode else {
        bail!(
            "ArceOS C qemu test build config {} must set `app-c = \"c\"` or another C source \
             directory",
            test.build_config_path.display()
        );
    };
    let artifacts = c_test_artifact_paths(
        &workspace_root,
        &test.build_group,
        &test.name,
        c_test_artifact_index(test),
    );

    let request = arceos.prepare_request(
        BuildCliArgs {
            config: Some(test.build_config_path.clone()),
            package: Some("ax-libc".to_string()),
            arch: None,
            target: Some(target.to_string()),
            plat_dyn: None,
            smp: None,
            debug: false,
        },
        None,
        None,
        SnapshotPersistence::Discard,
    )?;
    let cargo = build::load_c_app_cargo_config(&request)?;
    let input = c_test_build_input(
        app_dir,
        app_name,
        artifacts.target_dir,
        artifacts.out_dir,
        &test.name,
        build_config.build_info.features.clone(),
    );
    let output = cbuild::build_c_app(&workspace_root, &request, &input)?;
    let mut qemu = qemu_config;
    qemu_test::apply_dynamic_x86_64_qemu_boot(&mut qemu, &cargo);
    ensure_qemu_runtime_assets(arceos.app.workspace_root(), &qemu)?;
    let _host_http_server = qemu_test::load_qemu_case_host_http_server(&test.qemu_config_path)?
        .as_ref()
        .map(|config| HostHttpServerGuard::start(config, &test.name))
        .transpose()?;
    arceos
        .app
        .prepare_elf_artifact(output.elf_path, qemu.to_bin)
        .await?;
    arceos.app.run_prepared_qemu(qemu, None).await
}

fn c_test_build_input(
    app_dir: PathBuf,
    app_name: String,
    target_dir: PathBuf,
    out_dir: PathBuf,
    feature: &str,
    mut features: Vec<String>,
) -> cbuild::ArceosCBuildInput {
    features.push(format!("c-define:{}", c_test_feature_define(feature)));
    cbuild::ArceosCBuildInput {
        app_dir,
        app_name,
        target_dir,
        out_dir,
        features,
    }
}

fn c_test_feature_define(feature: &str) -> String {
    format!(
        "ARCEOS_C_TEST_CASE_{}",
        feature.replace('-', "_").to_ascii_uppercase()
    )
}

/// Returns isolated artifact paths for a single C test invocation.
///
/// Cases under the same build wrapper share a Cargo target dir and therefore
/// reuse the same ax-libc static library. QEMU output stays isolated per case
/// and per invocation so generated ELF files do not overwrite each other.
fn c_test_artifact_paths(
    workspace_root: &Path,
    build_group: &str,
    test_name: &str,
    invocation_index: usize,
) -> CTestArtifactPaths {
    let root = crate::context::axbuild_tmp_dir(workspace_root)
        .join("arceos-c")
        .join(test_name.replace('/', "-"));
    CTestArtifactPaths {
        target_dir: crate::context::axbuild_tmp_dir(workspace_root)
            .join("arceos-c")
            .join(build_group.replace('/', "-"))
            .join("cargo"),
        out_dir: root.join(format!("out-{invocation_index}")),
    }
}

#[cfg(test)]
pub(crate) fn parse_target(
    arch: &Option<String>,
    target: &Option<String>,
) -> anyhow::Result<(String, String)> {
    qemu_test::parse_test_target(
        arch,
        target,
        "arceos qemu tests",
        &crate::context::supported_arches(),
        &crate::context::supported_targets(),
        crate::context::resolve_arceos_arch_and_target,
    )
}

#[cfg(test)]
mod tests {
    use clap::Parser;
    use tempfile::tempdir;

    use super::*;
    use crate::arceos::Command;

    #[test]
    fn command_parses_test_qemu() {
        #[derive(Parser)]
        struct Cli {
            #[command(subcommand)]
            command: Command,
        }

        let cli =
            Cli::try_parse_from(["arceos", "test", "qemu", "--target", "x86_64-unknown-none"])
                .unwrap();

        match cli.command {
            Command::Test(args) => match args.command {
                TestCommand::Qemu(args) => {
                    assert_eq!(args.arch, None);
                    assert_eq!(args.target.as_deref(), Some("x86_64-unknown-none"));
                    assert!(args.package.is_empty());
                    assert!(!args.only_rust);
                    assert!(!args.only_c);
                }
            },
            _ => panic!("expected test command"),
        }
    }

    #[test]
    fn command_parses_test_qemu_only_rust() {
        #[derive(Parser)]
        struct Cli {
            #[command(subcommand)]
            command: Command,
        }

        let cli = Cli::try_parse_from([
            "arceos",
            "test",
            "qemu",
            "--target",
            "x86_64-unknown-none",
            "--only-rust",
        ])
        .unwrap();

        match cli.command {
            Command::Test(args) => match args.command {
                TestCommand::Qemu(args) => {
                    assert_eq!(args.arch, None);
                    assert_eq!(args.target.as_deref(), Some("x86_64-unknown-none"));
                    assert!(args.package.is_empty());
                    assert!(args.only_rust);
                    assert!(!args.only_c);
                }
            },
            _ => panic!("expected test command"),
        }
    }

    #[test]
    fn command_parses_test_qemu_only_c() {
        #[derive(Parser)]
        struct Cli {
            #[command(subcommand)]
            command: Command,
        }

        let cli = Cli::try_parse_from([
            "arceos",
            "test",
            "qemu",
            "--target",
            "x86_64-unknown-none",
            "--only-c",
        ])
        .unwrap();

        match cli.command {
            Command::Test(args) => match args.command {
                TestCommand::Qemu(args) => {
                    assert_eq!(args.arch, None);
                    assert_eq!(args.target.as_deref(), Some("x86_64-unknown-none"));
                    assert!(args.package.is_empty());
                    assert!(!args.only_rust);
                    assert!(args.only_c);
                }
            },
            _ => panic!("expected test command"),
        }
    }

    #[test]
    fn command_rejects_both_only_flags() {
        #[derive(Parser)]
        struct Cli {
            #[command(subcommand)]
            command: Command,
        }

        let result = Cli::try_parse_from([
            "arceos",
            "test",
            "qemu",
            "--target",
            "x86_64-unknown-none",
            "--only-rust",
            "--only-c",
        ]);
        assert!(result.is_err());
    }

    #[test]
    fn command_parses_removed_test_qemu_package_filter() {
        #[derive(Parser)]
        struct Cli {
            #[command(subcommand)]
            command: Command,
        }

        let cli = Cli::try_parse_from([
            "arceos",
            "test",
            "qemu",
            "--target",
            "riscv64gc-unknown-none-elf",
            "--package",
            "arceos-test-suit",
        ])
        .unwrap();

        match cli.command {
            Command::Test(args) => match args.command {
                TestCommand::Qemu(args) => {
                    assert_eq!(args.arch, None);
                    assert_eq!(args.target.as_deref(), Some("riscv64gc-unknown-none-elf"));
                    assert_eq!(args.package, vec!["arceos-test-suit".to_string()]);
                    let err = reject_removed_rust_package_filter(&args).unwrap_err();
                    assert!(err.to_string().contains("no longer support --package"));
                    assert!(!args.only_rust);
                    assert!(!args.only_c);
                }
            },
            _ => panic!("expected test command"),
        }
    }

    #[test]
    fn accepts_supported_targets() {
        assert_eq!(
            parse_target(&None, &Some("x86_64-unknown-none".to_string())).unwrap(),
            ("x86_64".to_string(), "x86_64-unknown-none".to_string())
        );
        assert_eq!(
            parse_target(&None, &Some("aarch64-unknown-none-softfloat".to_string())).unwrap(),
            (
                "aarch64".to_string(),
                "aarch64-unknown-none-softfloat".to_string()
            )
        );
    }

    #[test]
    fn accepts_supported_arch_aliases() {
        assert_eq!(
            parse_target(&Some("x86_64".to_string()), &None).unwrap(),
            ("x86_64".to_string(), "x86_64-unknown-none".to_string())
        );
        assert_eq!(
            parse_target(&Some("aarch64".to_string()), &None).unwrap(),
            (
                "aarch64".to_string(),
                "aarch64-unknown-none-softfloat".to_string()
            )
        );
    }

    #[test]
    fn rejects_unsupported_targets() {
        let rejected_target = "mips64-unknown-none".to_string();
        let err = parse_target(&None, &Some(rejected_target.clone())).unwrap_err();
        assert!(err.to_string().contains(&rejected_target));
    }

    #[test]
    fn arceos_c_default_run_selects_all_feature_only() {
        let features = c_qemu_features_for_run(None).unwrap();
        assert_eq!(features, vec![ARCEOS_C_ALL_FEATURE]);
    }

    #[test]
    fn arceos_c_selected_case_is_exact_feature_name() {
        let features = c_qemu_features_for_list(Some("pthread-basic")).unwrap();
        assert_eq!(features, vec!["pthread-basic"]);
    }

    #[test]
    fn arceos_c_default_list_hides_all_feature() {
        let features = c_qemu_features_for_list(None).unwrap();

        assert_eq!(features, ARCEOS_C_QEMU_LISTED_CASES);
        assert!(!features.contains(&ARCEOS_C_ALL_FEATURE));
    }

    #[test]
    fn arceos_c_selected_case_rejects_old_directory_name() {
        let err = c_qemu_features_for_list(Some("pthread/pthread-basic")).unwrap_err();
        assert!(
            err.to_string()
                .contains("unknown ArceOS c qemu test feature `pthread/pthread-basic`")
        );
    }

    #[test]
    fn arceos_c_feature_define_names_are_stable() {
        assert_eq!(
            c_test_feature_define("pthread-basic"),
            "ARCEOS_C_TEST_CASE_PTHREAD_BASIC"
        );
        assert_eq!(
            c_test_feature_define(ARCEOS_C_ALL_FEATURE),
            "ARCEOS_C_TEST_CASE_ALL"
        );
    }

    #[test]
    fn arceos_c_build_input_adds_selected_feature_define() {
        let dir = tempdir().unwrap();
        let app_dir = dir.path().join("c");
        let input = c_test_build_input(
            app_dir.clone(),
            ARCEOS_C_TEST_BUILD_GROUP.to_string(),
            PathBuf::from("/tmp/target"),
            PathBuf::from("/tmp/out"),
            "pthread-basic",
            vec!["alloc".to_string()],
        );

        assert_eq!(input.app_name, ARCEOS_C_TEST_BUILD_GROUP);
        assert_eq!(input.app_dir, app_dir);
        assert!(
            input
                .features
                .iter()
                .any(|feature| feature == "c-define:ARCEOS_C_TEST_CASE_PTHREAD_BASIC")
        );
    }

    #[test]
    fn arceos_c_qemu_case_uses_single_test_suite_paths() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("c")).unwrap();
        std::fs::write(root.join("c/main.c"), "int main(void) { return 0; }\n").unwrap();
        std::fs::write(
            root.join("build-x86_64-unknown-none.toml"),
            "features = []\n",
        )
        .unwrap();
        std::fs::write(
            root.join("qemu-x86_64.toml"),
            "args = [\"-nographic\"]\nuefi = false\nto_bin = false\nsuccess_regex = \
             [\"PASS\"]\nfail_regex = [\"panic\"]\n",
        )
        .unwrap();

        let case = load_arceos_c_test_suit_qemu_case(root, "x86_64", "x86_64-unknown-none", "mem")
            .unwrap();

        assert_eq!(case.name, "mem");
        assert_eq!(case.build_group, ARCEOS_C_TEST_BUILD_GROUP);
        assert!(
            case.build_config_path
                .ends_with("build-x86_64-unknown-none.toml")
        );
        assert!(case.qemu_config_path.ends_with("qemu-x86_64.toml"));
    }

    #[test]
    fn load_c_test_build_config_reads_build_info() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("build-x86_64-unknown-none.toml");
        std::fs::write(
            &path,
            "app-c = \"c\"\nfeatures = [\"alloc\", \"paging\"]\nlog = \"Trace\"\nmax_cpu_num = \
             4\n\n[env]\n",
        )
        .unwrap();

        let config = load_c_test_build_config(&path).unwrap();
        assert_eq!(config.app_c, Some(PathBuf::from("c")));
        assert_eq!(config.build_info.features, vec!["alloc", "paging"]);
        assert_eq!(config.build_info.log, build::LogLevel::Trace);
        assert_eq!(config.build_info.max_cpu_num, Some(4));
    }

    #[test]
    fn load_c_test_build_config_rejects_missing_app_c() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("build-x86_64-unknown-none.toml");
        std::fs::write(&path, "features = [\"alloc\"]\nlog = \"Info\"\n\n[env]\n").unwrap();

        let err = load_c_test_build_config(&path).unwrap_err();
        assert!(err.to_string().contains("must set `app-c = \"c\"`"));
    }

    #[test]
    fn load_c_test_qemu_config_reads_standard_qemu_config() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("qemu-x86_64.toml");
        std::fs::write(
            &path,
            "args = [\"-nographic\"]\nuefi = false\nto_bin = false\nsuccess_regex = \
             [\"PASS\"]\nfail_regex = [\"panic\"]\ntimeout = 120\n",
        )
        .unwrap();

        let config = load_c_test_qemu_config(&path).unwrap();
        assert_eq!(config.args, vec!["-nographic"]);
        assert_eq!(config.success_regex, vec!["PASS"]);
        assert_eq!(config.fail_regex, vec!["panic"]);
        assert_eq!(config.timeout, Some(120));
    }

    #[test]
    fn selected_qemu_test_groups_default_runs_rust_then_c() {
        let dir = tempdir().unwrap();
        let flows = selected_qemu_test_groups(
            dir.path(),
            &ArgsTestQemu {
                arch: None,
                target: Some("x86_64-unknown-none".to_string()),
                test_group: None,
                test_case: None,
                list: false,
                package: Vec::new(),
                only_rust: false,
                only_c: false,
                no_symbolize: false,
                keep_qemu_log: false,
            },
        )
        .unwrap();

        assert_eq!(flows, &[QemuTestFlow::Rust, QemuTestFlow::C]);
    }

    #[test]
    fn selected_qemu_test_groups_only_rust_skips_c() {
        let dir = tempdir().unwrap();
        let flows = selected_qemu_test_groups(
            dir.path(),
            &ArgsTestQemu {
                arch: None,
                target: Some("x86_64-unknown-none".to_string()),
                test_group: None,
                test_case: None,
                list: false,
                package: Vec::new(),
                only_rust: true,
                only_c: false,
                no_symbolize: false,
                keep_qemu_log: false,
            },
        )
        .unwrap();

        assert_eq!(flows, &[QemuTestFlow::Rust]);
    }

    #[test]
    fn selected_qemu_test_groups_only_c_skips_rust() {
        let dir = tempdir().unwrap();
        let flows = selected_qemu_test_groups(
            dir.path(),
            &ArgsTestQemu {
                arch: None,
                target: Some("x86_64-unknown-none".to_string()),
                test_group: None,
                test_case: None,
                list: false,
                package: Vec::new(),
                only_rust: false,
                only_c: true,
                no_symbolize: false,
                keep_qemu_log: false,
            },
        )
        .unwrap();

        assert_eq!(flows, &[QemuTestFlow::C]);
    }

    #[test]
    fn selected_qemu_test_groups_package_filter_no_longer_changes_groups() {
        let dir = tempdir().unwrap();
        let flows = selected_qemu_test_groups(
            dir.path(),
            &ArgsTestQemu {
                arch: None,
                target: Some("x86_64-unknown-none".to_string()),
                test_group: None,
                test_case: None,
                list: false,
                package: vec!["arceos-test-suit".to_string()],
                only_rust: false,
                only_c: false,
                no_symbolize: false,
                keep_qemu_log: false,
            },
        )
        .unwrap();

        assert_eq!(flows, &[QemuTestFlow::Rust, QemuTestFlow::C]);
    }

    #[test]
    fn arceos_rust_qemu_test_uses_single_test_suite_package() {
        let app_dir = tempfile::tempdir().unwrap();
        let build_config = app_dir.path().join("build-x86_64-unknown-none.toml");
        fs::write(&build_config, "features = [\"ax-std\"]\n").unwrap();

        let args = test_build_args(
            ARCEOS_RUST_TEST_PACKAGE,
            "x86_64-unknown-none",
            &build_config,
        );

        assert_eq!(args.config, Some(build_config));
        assert_eq!(args.package.as_deref(), Some(ARCEOS_RUST_TEST_PACKAGE));
        assert_eq!(args.target.as_deref(), Some("x86_64-unknown-none"));
    }

    #[test]
    fn arceos_rust_default_run_selects_all_feature_only() {
        let features = rust_qemu_features_for_run(None, false).unwrap();
        assert_eq!(features, vec![ARCEOS_RUST_ALL_FEATURE]);
    }

    #[test]
    fn arceos_rust_selected_case_is_feature_name() {
        let features = rust_qemu_features_for_list(Some("task-yield"), false).unwrap();
        assert_eq!(features, vec!["task-yield"]);
    }

    #[test]
    fn arceos_rust_selected_cases_include_restored_coverage_features() {
        for feature in [
            ARCEOS_RUST_DEBUG_BACKTRACE_FEATURE,
            ARCEOS_RUST_DEBUG_PANIC_PATH_FEATURE,
            ARCEOS_RUST_EXCEPTION_PAGE_FAULT_FEATURE,
            "fs-basic",
            "lockdep-baseline",
            ARCEOS_RUST_LOCKDEP_DETECT_FEATURE,
            "net-loopback",
            "sched-cfs",
            "sched-rr",
            ARCEOS_RUST_STACK_GUARD_PAGE_FEATURE,
        ] {
            let features = rust_qemu_features_for_list(Some(feature), false).unwrap();
            assert_eq!(features, vec![feature]);
        }
    }

    #[test]
    fn arceos_rust_debug_backtrace_requires_symbolized_frames() {
        let regexes =
            rust_qemu_host_symbolize_success_regex(Some(ARCEOS_RUST_DEBUG_BACKTRACE_FEATURE));
        assert_eq!(regexes.len(), 2);

        let output = r#"
BACKTRACE_BLOCK 0 kind=arceos-test-suit-raw-normal arch=x86_64
BT 0 ip=0x10 fp=0x20 arceos_test_suit::debug::backtrace::nested_c
BT 1 ip=0x11 fp=0x21 arceos_test_suit::debug::backtrace::nested_b
BT 2 ip=0x12 fp=0x22 arceos_test_suit::debug::backtrace::nested_a
BACKTRACE_BLOCK 1 kind=arceos-test-suit-raw-badfp arch=x86_64
BT 0 ip=0x1 fp=0x2
"#;
        for pattern in &regexes {
            assert!(Regex::new(pattern).unwrap().is_match(output));
        }
    }

    #[test]
    fn arceos_rust_page_fault_qemu_uses_page_fault_result_regex() {
        let mut cargo = rust_test_cargo_for_target("x86_64-unknown-none");
        let mut qemu = QemuConfig {
            success_regex: vec!["ArceOS test suite run OK!".to_string()],
            fail_regex: vec![r"(?i)\bpanic(?:ked)?\b".to_string()],
            timeout: Some(60),
            ..QemuConfig::default()
        };

        apply_rust_qemu_feature_overrides(
            &mut cargo,
            &mut qemu,
            Some(ARCEOS_RUST_EXCEPTION_PAGE_FAULT_FEATURE),
        );

        assert_eq!(qemu.success_regex, vec!["Page fault test OK!"]);
        assert_eq!(
            qemu.fail_regex,
            vec![
                r"(?i)\bpanic(?:ked)?\b",
                "page fault handler did not stop the system"
            ]
        );
        assert_eq!(qemu.timeout, Some(30));
    }

    #[test]
    fn arceos_rust_stack_guard_page_qemu_uses_guard_page_result_regex() {
        let mut cargo = rust_test_cargo_for_target("x86_64-unknown-none");
        let mut qemu = QemuConfig {
            success_regex: vec!["ArceOS test suite run OK!".to_string()],
            fail_regex: vec![
                r"(?i)\bpanic(?:ked)?\b".to_string(),
                "ARCEOS_TEST_FAIL".to_string(),
            ],
            timeout: Some(60),
            ..QemuConfig::default()
        };

        apply_rust_qemu_feature_overrides(
            &mut cargo,
            &mut qemu,
            Some(ARCEOS_RUST_STACK_GUARD_PAGE_FEATURE),
        );

        assert_eq!(
            qemu.success_regex,
            vec!["task stack guard page hit for .*stack-guard-page-overflow"]
        );
        assert_eq!(qemu.fail_regex, vec!["stack guard page was not hit"]);
        assert_eq!(qemu.timeout, Some(30));
    }

    #[test]
    fn arceos_rust_panic_path_qemu_uses_panic_backtrace_result_regex() {
        let mut cargo = rust_test_cargo_for_target("x86_64-unknown-none");
        let mut qemu = QemuConfig {
            success_regex: vec!["ArceOS test suite run OK!".to_string()],
            fail_regex: vec![r"(?i)\bpanic(?:ked)?\b".to_string()],
            timeout: Some(60),
            ..QemuConfig::default()
        };

        apply_rust_qemu_feature_overrides(
            &mut cargo,
            &mut qemu,
            Some(ARCEOS_RUST_DEBUG_PANIC_PATH_FEATURE),
        );

        assert_eq!(
            qemu.success_regex,
            vec![r"BACKTRACE_BEGIN\b.*\bkind=panic\b"]
        );
        assert_eq!(qemu.fail_regex, vec!["ARCEOS_TEST_FAIL"]);
        assert_eq!(qemu.timeout, Some(30));
    }

    #[test]
    fn arceos_rust_lockdep_detect_qemu_uses_lockdep_result_regex() {
        let mut cargo = rust_test_cargo_for_target("x86_64-unknown-none");
        let mut qemu = QemuConfig {
            success_regex: vec!["ArceOS test suite run OK!".to_string()],
            fail_regex: vec![r"(?i)\bpanic(?:ked)?\b".to_string()],
            timeout: Some(60),
            ..QemuConfig::default()
        };

        apply_rust_qemu_feature_overrides(
            &mut cargo,
            &mut qemu,
            Some(ARCEOS_RUST_LOCKDEP_DETECT_FEATURE),
        );

        assert_eq!(
            qemu.success_regex,
            vec!["lockdep: lock order inversion detected"]
        );
        assert_eq!(
            qemu.fail_regex,
            vec!["lockdep did not report an expected lock order inversion"]
        );
        assert_eq!(qemu.timeout, Some(30));
    }

    #[test]
    fn arceos_rust_remote_wake_riscv_uses_single_threaded_tcg() {
        let mut cargo = rust_test_cargo_for_target("riscv64gc-unknown-none-elf");
        let mut qemu = QemuConfig::default();

        apply_rust_qemu_feature_overrides(
            &mut cargo,
            &mut qemu,
            Some("task-wait-queue-remote-wake"),
        );

        assert!(
            qemu.args
                .windows(2)
                .any(|args| args == ["-accel", "tcg,thread=single"])
        );
    }

    #[test]
    fn arceos_rust_normal_qemu_keeps_suite_result_regex() {
        let mut cargo = rust_test_cargo_for_target("x86_64-unknown-none");
        let mut qemu = QemuConfig {
            success_regex: vec!["ArceOS test suite run OK!".to_string()],
            fail_regex: vec![
                r"(?i)\bpanic(?:ked)?\b".to_string(),
                "ARCEOS_TEST_FAIL".to_string(),
            ],
            timeout: Some(60),
            ..QemuConfig::default()
        };

        apply_rust_qemu_feature_overrides(&mut cargo, &mut qemu, Some("debug-backtrace"));

        assert_eq!(qemu.success_regex, vec!["ArceOS test suite run OK!"]);
        assert_eq!(
            qemu.fail_regex,
            vec![r"(?i)\bpanic(?:ked)?\b", "ARCEOS_TEST_FAIL"]
        );
        assert_eq!(qemu.timeout, Some(60));
    }

    #[test]
    fn arceos_rust_selected_case_rejects_old_path_name() {
        let err = rust_qemu_features_for_list(Some("task/yield"), false).unwrap_err();
        assert!(
            err.to_string()
                .contains("unknown ArceOS rust qemu test feature `task/yield`")
        );
    }

    #[test]
    fn arceos_rust_selected_case_can_miss_in_default_group_search() {
        let features = rust_qemu_features_for_list(Some("c/helloworld"), true).unwrap();
        assert!(features.is_empty());
    }

    #[test]
    fn arceos_qemu_build_identity_includes_feature() {
        let build_config = PathBuf::from("/tmp/arceos/build-x86_64-unknown-none.toml");
        let cases = vec![
            prepared_arceos_qemu_case("one", "feature-one", &build_config),
            prepared_arceos_qemu_case("two", "feature-two", &build_config),
            prepared_arceos_qemu_case("one/again", "feature-one", &build_config),
        ];

        let groups = group_arceos_qemu_cases_by_build_identity(&cases);

        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].package, ARCEOS_RUST_TEST_PACKAGE);
        assert_eq!(groups[0].feature, Some("feature-one"));
        assert_eq!(groups[0].cases.len(), 2);
        assert_eq!(groups[1].package, ARCEOS_RUST_TEST_PACKAGE);
        assert_eq!(groups[1].feature, Some("feature-two"));
        assert_eq!(groups[1].cases.len(), 1);
    }

    fn prepared_arceos_qemu_case(
        name: &str,
        feature: &str,
        build_config_path: &Path,
    ) -> PreparedArceosRustQemuCase {
        PreparedArceosRustQemuCase {
            case: ArceosRustQemuCase {
                case: TestQemuCase {
                    name: name.to_string(),
                    display_name: name.to_string(),
                    case_dir: PathBuf::from(format!("/tmp/{name}")),
                    qemu_config_path: PathBuf::from(format!("/tmp/{name}/qemu-x86_64.toml")),
                    test_commands: Vec::new(),
                    host_symbolize_success_regex: Vec::new(),
                    host_http_server: None,
                    subcases: Vec::new(),
                },
                build_group: "std".to_string(),
                build_config_path: build_config_path.to_path_buf(),
                package: ARCEOS_RUST_TEST_PACKAGE.to_string(),
                feature: Some(feature.to_string()),
            },
            request: ResolvedBuildRequest {
                package: ARCEOS_RUST_TEST_PACKAGE.to_string(),
                arch: "x86_64".to_string(),
                target: "x86_64-unknown-none".to_string(),
                plat_dyn: None,
                smp: None,
                debug: false,
                build_info_path: build_config_path.to_path_buf(),
                qemu_config: None,
                uboot_config: None,
            },
            cargo: Cargo {
                env: Default::default(),
                target: "x86_64-unknown-none".to_string(),
                package: ARCEOS_RUST_TEST_PACKAGE.to_string(),
                features: vec![feature.to_string()],
                log: None,
                extra_config: None,
                profile: None,
                disable_someboot_build_config: true,
                args: Vec::new(),
                pre_build_cmds: Vec::new(),
                post_build_cmds: Vec::new(),
                to_bin: false,
                bin: None,
            },
            qemu: QemuConfig::default(),
            host_symbolize_success_regex: Vec::new(),
        }
    }

    fn rust_test_cargo_for_target(target: &str) -> Cargo {
        Cargo {
            env: Default::default(),
            target: target.to_string(),
            package: ARCEOS_RUST_TEST_PACKAGE.to_string(),
            features: Vec::new(),
            log: None,
            extra_config: None,
            profile: None,
            disable_someboot_build_config: true,
            args: Vec::new(),
            pre_build_cmds: Vec::new(),
            post_build_cmds: Vec::new(),
            to_bin: false,
            bin: None,
        }
    }
}
