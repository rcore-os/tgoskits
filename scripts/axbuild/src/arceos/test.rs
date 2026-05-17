use std::{
    fs,
    path::{Path, PathBuf},
    time::Instant,
};

use anyhow::{Context, bail};
use clap::{Args, Subcommand};
use ostool::{build::config::Cargo, run::qemu::QemuConfig};
use serde::Deserialize;

use super::{ArceOS, build, cbuild, ensure_qemu_runtime_assets};
use crate::{
    context::{BuildCliArgs, ResolvedBuildRequest, SnapshotPersistence},
    test::{case::TestQemuCase, qemu as qemu_test, suite as test_suite},
};

const ARCEOS_RUST_TEST_GROUP: &str = "rust";
const ARCEOS_C_TEST_GROUP: &str = "c";
const ARCEOS_TEST_SUITE_OS: &str = "arceos";

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
    /// Only run the specified Rust test package(s)
    #[arg(short, long, value_name = "PACKAGE", conflicts_with = "only_c")]
    pub package: Vec<String>,
    /// Only run Rust tests; prefer `--test-group rust`
    #[arg(long, conflicts_with = "only_c", hide = true)]
    pub only_rust: bool,
    /// Only run C tests; prefer `--test-group c`
    #[arg(long, conflicts_with = "only_rust", hide = true)]
    pub only_c: bool,
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
}

#[derive(Debug, Clone)]
struct PreparedArceosRustQemuCase {
    case: ArceosRustQemuCase,
    request: ResolvedBuildRequest,
    cargo: Cargo,
    qemu: QemuConfig,
}

struct ArceosQemuBuildGroup<'a> {
    build_group: &'a str,
    build_config_path: &'a Path,
    package: &'a str,
    request: ResolvedBuildRequest,
    cargo: Cargo,
    cases: Vec<&'a PreparedArceosRustQemuCase>,
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
    case_dir: PathBuf,
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

#[derive(Debug, Clone, Deserialize)]
struct CTestBuildConfig {
    #[serde(flatten)]
    build: build::ArceosBuildInfo,
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
        let mut trees = Vec::new();
        for group in groups {
            match group {
                QemuTestFlow::Rust => trees.extend(list_rust_qemu_cases(
                    arceos,
                    None,
                    args.test_case.as_deref(),
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

    let selected_case = resolve_rust_selected_case(arceos, &args)?;
    let (arch, target) = qemu_test::parse_test_target(
        &args.arch,
        &args.target,
        "arceos qemu tests",
        &crate::context::supported_arches(),
        &crate::context::supported_targets(),
        crate::context::resolve_arceos_arch_and_target,
    )?;
    let groups = selected_qemu_test_groups(arceos.app.workspace_root(), &args)?;
    if args.list {
        let mut trees = Vec::new();
        for group in groups {
            match group {
                QemuTestFlow::Rust => trees.extend(list_rust_qemu_cases(
                    arceos,
                    Some((&arch, &target)),
                    selected_case.as_deref(),
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
                    selected_case.as_deref(),
                )?),
            }
        }
        if trees.is_empty() {
            bail!("no ArceOS qemu test cases found");
        }
        println!("{}", trees.join("\n"));
        return Ok(());
    }

    for flow in groups {
        match flow {
            QemuTestFlow::Rust => {
                test_rust_qemu(arceos, &arch, &target, selected_case.as_deref()).await?
            }
            QemuTestFlow::C => test_c_qemu(arceos, &target, args.test_case.as_deref()).await?,
            QemuTestFlow::Generic(ref group) => {
                test_generic_qemu(arceos, &arch, &target, group, selected_case.as_deref()).await?
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
) -> anyhow::Result<()> {
    let cases = discover_rust_qemu_cases(arceos, arch, target, selected_case)?;
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
                format!(
                    "failed to build ArceOS rust qemu test artifact for package `{}` in build \
                     group `{}` ({})",
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
            let result = run_rust_qemu_case(arceos, case)
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
    selected_case: Option<&str>,
) -> anyhow::Result<()> {
    let dir = arceos_test_group_dir(arceos.app.workspace_root(), group);
    let cases = discover_qemu_cases_in_dir(&dir, arch, target, selected_case, group)?;
    let group_label = format!("arceos {group}");
    println!(
        "running {group_label} qemu tests for arch: {arch} (target: {target}, cases: {})",
        cases.len()
    );
    let prepared = prepare_rust_qemu_cases(arceos, target, cases).await?;
    let total = prepared.len();

    run_generic_qemu_by_build_group(arceos, group, &group_label, &prepared, total).await
}

async fn run_generic_qemu_by_build_group(
    arceos: &mut ArceOS,
    group: &str,
    group_label: &str,
    prepared: &[PreparedArceosRustQemuCase],
    total: usize,
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
                format!(
                    "failed to build ArceOS {group} qemu test artifact for package `{}` in build \
                     group `{}` ({})",
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
            let result = run_rust_qemu_case(arceos, case)
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
        }) {
            group.cases.push(case);
            continue;
        }

        groups.push(ArceosQemuBuildGroup {
            build_group: &case.case.build_group,
            build_config_path: &case.case.build_config_path,
            package: &case.case.package,
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
) -> anyhow::Result<Vec<ArceosRustQemuCase>> {
    discover_qemu_cases_in_dir(
        &arceos_rust_test_dir(arceos),
        arch,
        target,
        selected_case,
        ARCEOS_RUST_TEST_GROUP,
    )
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

fn load_rust_qemu_case(case: qemu_test::DiscoveredQemuCase) -> anyhow::Result<ArceosRustQemuCase> {
    let package = read_manifest_package_name(&case.case_dir.join("Cargo.toml"))?;
    Ok(ArceosRustQemuCase {
        case: TestQemuCase {
            name: case.name,
            display_name: case.display_name,
            case_dir: case.case_dir,
            qemu_config_path: case.qemu_config_path,
            test_commands: Vec::new(),
            subcases: Vec::new(),
        },
        build_group: case.build_group,
        build_config_path: case.build_config_path,
        package,
    })
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
        let cargo = build::load_cargo_config(&request)?;
        let qemu = arceos
            .load_qemu_config(&request, &cargo)
            .await?
            .with_context(|| {
                format!(
                    "failed to load ArceOS qemu config for case `{}`",
                    case.case.display_name
                )
            })?;
        ensure_qemu_runtime_assets(arceos.app.workspace_root(), &qemu)?;
        prepared.push(PreparedArceosRustQemuCase {
            case,
            request,
            cargo,
            qemu,
        });
    }
    Ok(prepared)
}

async fn run_rust_qemu_case(
    arceos: &mut ArceOS,
    case: &PreparedArceosRustQemuCase,
) -> anyhow::Result<()> {
    arceos.app.run_qemu(&case.cargo, case.qemu.clone()).await
}

fn list_rust_qemu_cases(
    arceos: &ArceOS,
    target: Option<(&str, &str)>,
    selected_case: Option<&str>,
) -> anyhow::Result<Option<String>> {
    let cases = rust_qemu_case_names(arceos, target, selected_case)?;
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
    let cases: Vec<String> = match target {
        Some((arch, target)) => discover_c_tests(
            &arceos_c_test_dir(arceos),
            Some(arch),
            Some(target),
            selected_case,
        )?
        .into_iter()
        .map(|case| case.name)
        .collect(),
        None => c_qemu_case_names(arceos, selected_case)?,
    };
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
            ARCEOS_RUST_TEST_GROUP => match rust_qemu_listed_cases(arceos, selected_case) {
                Ok(cases) if !cases.is_empty() => Some(cases),
                Ok(_) => None,
                Err(err) if qemu_list_error_is_ignorable(err.kind()) => None,
                Err(err) => return Err(anyhow::Error::new(err)),
            },
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
) -> qemu_test::ListQemuCasesResult<Vec<qemu_test::ListedQemuCase>> {
    qemu_test::discover_all_qemu_cases_with_archs(
        &arceos_rust_test_dir(arceos),
        selected_case,
        "ArceOS",
        ARCEOS_RUST_TEST_GROUP,
    )
}

fn rust_qemu_case_names(
    arceos: &ArceOS,
    target: Option<(&str, &str)>,
    selected_case: Option<&str>,
) -> anyhow::Result<Vec<String>> {
    match target {
        Some((arch, target)) => Ok(
            discover_rust_qemu_cases(arceos, arch, target, selected_case)?
                .into_iter()
                .map(|case| case.case.name)
                .collect(),
        ),
        None => qemu_test::discover_all_qemu_cases(
            &arceos_rust_test_dir(arceos),
            selected_case,
            "ArceOS",
            ARCEOS_RUST_TEST_GROUP,
        )
        .map_err(anyhow::Error::new),
    }
}

fn c_qemu_case_names(arceos: &ArceOS, selected_case: Option<&str>) -> anyhow::Result<Vec<String>> {
    let tests = discover_c_tests(&arceos_c_test_dir(arceos), None, None, selected_case)?;
    Ok(tests.into_iter().map(|test| test.name).collect())
}

fn c_qemu_listed_cases(
    arceos: &ArceOS,
    selected_case: Option<&str>,
) -> qemu_test::ListQemuCasesResult<Vec<qemu_test::ListedQemuCase>> {
    qemu_test::discover_all_qemu_cases_with_archs(
        &arceos_c_test_dir(arceos),
        selected_case,
        "ArceOS",
        ARCEOS_C_TEST_GROUP,
    )
}

fn resolve_rust_selected_case(
    arceos: &ArceOS,
    args: &ArgsTestQemu,
) -> anyhow::Result<Option<String>> {
    if args.package.is_empty() {
        return Ok(args.test_case.clone());
    }

    if args.package.len() > 1 {
        bail!("ArceOS --package compatibility mode accepts one package at a time");
    }
    if args.test_case.is_some() {
        bail!("ArceOS --package cannot be combined with --test-case");
    }
    let package = &args.package[0];
    let case_name = rust_case_name_for_package(arceos, package)?;
    Ok(Some(case_name))
}

fn rust_case_name_for_package(arceos: &ArceOS, package: &str) -> anyhow::Result<String> {
    let root = arceos_rust_test_dir(arceos);
    let mut stack = vec![root.clone()];
    while let Some(dir) = stack.pop() {
        let manifest = dir.join("Cargo.toml");
        if manifest.is_file() && read_manifest_package_name(&manifest)? == package {
            return dir
                .strip_prefix(&root)
                .map(|path| {
                    path.components()
                        .map(|component| component.as_os_str().to_string_lossy())
                        .collect::<Vec<_>>()
                        .join("/")
                })
                .with_context(|| {
                    format!("failed to derive ArceOS rust case name for package `{package}`")
                });
        }

        for entry in
            fs::read_dir(&dir).with_context(|| format!("failed to read {}", dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            }
        }
    }

    bail!(
        "unsupported arceos rust test package `{package}`; expected a Cargo.toml package under {}",
        root.display()
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
    if args.only_rust || !args.package.is_empty() {
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

fn discover_c_tests(
    c_test_root: &Path,
    arch: Option<&str>,
    target: Option<&str>,
    selected_case: Option<&str>,
) -> anyhow::Result<Vec<CTestDef>> {
    if let (Some(arch), Some(target)) = (arch, target) {
        let tests = qemu_test::discover_qemu_cases(
            c_test_root,
            arch,
            target,
            None,
            "ArceOS",
            ARCEOS_C_TEST_GROUP,
        )?
        .into_iter()
        .map(load_c_test)
        .collect::<anyhow::Result<Vec<_>>>()?;
        return filter_c_tests(tests, selected_case);
    }

    Ok(qemu_test::discover_all_qemu_cases(
        c_test_root,
        selected_case,
        "ArceOS",
        ARCEOS_C_TEST_GROUP,
    )?
    .into_iter()
    .map(|name| CTestDef {
        name: name.clone(),
        build_group: name,
        case_dir: PathBuf::new(),
        build_config_path: PathBuf::new(),
        qemu_config_path: PathBuf::new(),
    })
    .collect())
}

fn filter_c_tests(
    tests: Vec<CTestDef>,
    selected_case: Option<&str>,
) -> anyhow::Result<Vec<CTestDef>> {
    let Some(selected_case) = selected_case else {
        return Ok(tests);
    };
    let selected_prefix = format!("{selected_case}/");
    let selected = tests
        .into_iter()
        .filter(|test| test.name == selected_case || test.name.starts_with(&selected_prefix))
        .collect::<Vec<_>>();
    if selected.is_empty() {
        bail!("unknown ArceOS c qemu test case `{selected_case}`");
    }
    Ok(selected)
}

fn load_c_test(case: qemu_test::DiscoveredQemuCase) -> anyhow::Result<CTestDef> {
    Ok(CTestDef {
        name: case.name,
        build_group: case.build_group,
        case_dir: case.case_dir,
        build_config_path: case.build_config_path,
        qemu_config_path: case.qemu_config_path,
    })
}

fn load_c_test_build_config(path: &Path) -> anyhow::Result<CTestBuildConfig> {
    toml::from_str(&fs::read_to_string(path)?)
        .with_context(|| format!("failed to parse C build config {}", path.display()))
}

fn load_c_test_qemu_config(path: &Path) -> anyhow::Result<QemuConfig> {
    toml::from_str(&fs::read_to_string(path)?)
        .with_context(|| format!("failed to parse C qemu config {}", path.display()))
}

fn resolve_c_test_source_dir(case_dir: &Path) -> anyhow::Result<PathBuf> {
    let source_dir = case_dir.join("c");
    if source_dir.is_dir() && dir_has_c_source(&source_dir)? {
        return source_dir
            .canonicalize()
            .with_context(|| format!("failed to resolve C source dir {}", source_dir.display()));
    }

    bail!(
        "ArceOS C qemu test case {} must contain a c/ source asset directory with .c files",
        case_dir.display()
    )
}

fn c_test_app_name(source_dir: &Path, fallback: &str) -> String {
    let app_dir = if source_dir.file_name().and_then(|name| name.to_str()) == Some("c") {
        source_dir.parent().unwrap_or(source_dir)
    } else {
        source_dir
    };
    app_dir
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(fallback)
        .to_string()
}

fn dir_has_c_source(dir: &Path) -> anyhow::Result<bool> {
    Ok(fs::read_dir(dir)
        .with_context(|| format!("failed to read {}", dir.display()))?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .any(|entry| entry.path().extension().is_some_and(|ext| ext == "c")))
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
    let workspace_root = arceos.app.workspace_root().to_path_buf();
    let c_test_root = arceos_test_group_dir(&workspace_root, ARCEOS_C_TEST_GROUP);
    let c_tests = discover_c_tests(&c_test_root, Some(arch), Some(target), selected_case)?;
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
    let source_dir = resolve_c_test_source_dir(&test.case_dir)?;
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
    let input = cbuild::ArceosCBuildInput {
        app_dir: source_dir.clone(),
        app_name: c_test_app_name(&source_dir, &test.name),
        target_dir: artifacts.target_dir,
        out_dir: artifacts.out_dir,
        features: build_config.build.features.clone(),
    };
    let output = cbuild::build_c_app(&workspace_root, &request, &input)?;
    let qemu = qemu_config;
    ensure_qemu_runtime_assets(arceos.app.workspace_root(), &qemu)?;
    arceos
        .app
        .prepare_elf_artifact(output.elf_path, qemu.to_bin)
        .await?;
    arceos.app.run_prepared_qemu(qemu).await
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
    fn command_parses_test_qemu_package_filter() {
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
            "arceos-ipi",
        ])
        .unwrap();

        match cli.command {
            Command::Test(args) => match args.command {
                TestCommand::Qemu(args) => {
                    assert_eq!(args.arch, None);
                    assert_eq!(args.target.as_deref(), Some("riscv64gc-unknown-none-elf"));
                    assert_eq!(args.package, vec!["arceos-ipi".to_string()]);
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

    fn write_c_case(dir: &Path, name: &str, qemu_arch: &str) {
        let case_dir = dir.join(name);
        let source_dir = case_dir.join("c");
        std::fs::create_dir_all(&source_dir).unwrap();
        std::fs::write(source_dir.join("main.c"), "int main(void) { return 0; }\n").unwrap();
        std::fs::write(
            case_dir.join("build-x86_64-unknown-none.toml"),
            "features = [\"alloc\"]\nlog = \"Info\"\n",
        )
        .unwrap();
        std::fs::write(
            case_dir.join(format!("qemu-{qemu_arch}.toml")),
            "args = [\"-nographic\"]\nuefi = false\nto_bin = false\nsuccess_regex = [\"Shutting \
             down\"]\nfail_regex = [\"panic\"]\n",
        )
        .unwrap();
    }

    #[test]
    fn discover_c_tests_uses_build_and_qemu_toml() {
        let dir = tempdir().unwrap();
        write_c_case(dir.path(), "helloworld", "x86_64");
        write_c_case(dir.path(), "pthread/basic", "x86_64");
        std::fs::create_dir_all(dir.path().join("helpers")).unwrap();
        std::fs::write(
            dir.path().join("helpers/helper.c"),
            "void helper(void) {}\n",
        )
        .unwrap();

        let tests = discover_c_tests(
            dir.path(),
            Some("x86_64"),
            Some("x86_64-unknown-none"),
            None,
        )
        .unwrap();
        assert_eq!(
            tests
                .iter()
                .map(|test| test.name.as_str())
                .collect::<Vec<_>>(),
            vec!["helloworld", "pthread/basic"]
        );
        assert!(
            tests[0]
                .build_config_path
                .ends_with("build-x86_64-unknown-none.toml")
        );
        assert!(tests[0].qemu_config_path.ends_with("qemu-x86_64.toml"));
    }

    #[test]
    fn load_c_test_build_config_reads_build_info() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("build-x86_64-unknown-none.toml");
        std::fs::write(
            &path,
            "features = [\"alloc\", \"paging\"]\nlog = \"Trace\"\nmax_cpu_num = 4\n\n[env]\n",
        )
        .unwrap();

        let config = load_c_test_build_config(&path).unwrap();
        assert_eq!(config.build.features, vec!["alloc", "paging"]);
        assert_eq!(config.build.log, build::LogLevel::Trace);
        assert_eq!(config.build.max_cpu_num, Some(4));
    }

    #[test]
    fn resolve_c_test_source_dir_accepts_case_c_asset_dir() {
        let dir = tempdir().unwrap();
        let case_dir = dir.path().join("helloworld");
        let source_dir = case_dir.join("c");
        std::fs::create_dir_all(&source_dir).unwrap();
        std::fs::write(source_dir.join("main.c"), "int main(void) { return 0; }\n").unwrap();

        assert_eq!(
            resolve_c_test_source_dir(&case_dir).unwrap(),
            source_dir.canonicalize().unwrap()
        );
    }

    #[test]
    fn resolve_c_test_source_dir_rejects_direct_c_sources() {
        let dir = tempdir().unwrap();
        let case_dir = dir.path().join("helloworld");
        std::fs::create_dir_all(&case_dir).unwrap();
        std::fs::write(case_dir.join("main.c"), "int main(void) { return 0; }\n").unwrap();

        let err = resolve_c_test_source_dir(&case_dir).unwrap_err();
        assert!(err.to_string().contains("c/ source asset directory"));
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
            },
        )
        .unwrap();

        assert_eq!(flows, &[QemuTestFlow::C]);
    }

    #[test]
    fn selected_qemu_test_groups_package_filter_runs_only_rust() {
        let dir = tempdir().unwrap();
        let flows = selected_qemu_test_groups(
            dir.path(),
            &ArgsTestQemu {
                arch: None,
                target: Some("x86_64-unknown-none".to_string()),
                test_group: None,
                test_case: None,
                list: false,
                package: vec!["arceos-ipi".to_string()],
                only_rust: false,
                only_c: false,
            },
        )
        .unwrap();

        assert_eq!(flows, &[QemuTestFlow::Rust]);
    }

    #[test]
    fn arceos_rust_qemu_test_uses_case_build_config() {
        let app_dir = tempfile::tempdir().unwrap();
        let build_config = app_dir.path().join("build-x86_64-unknown-none.toml");
        fs::write(&build_config, "features = [\"ax-std\"]\n").unwrap();

        let args = test_build_args("arceos-lockdep", "x86_64-unknown-none", &build_config);

        assert_eq!(args.config, Some(build_config));
        assert_eq!(args.package.as_deref(), Some("arceos-lockdep"));
        assert_eq!(args.target.as_deref(), Some("x86_64-unknown-none"));
    }

    #[test]
    fn arceos_qemu_build_identity_includes_package() {
        let build_config = PathBuf::from("/tmp/arceos/build-x86_64-unknown-none.toml");
        let cases = vec![
            prepared_arceos_qemu_case("one", "test-arceos-std-one", &build_config),
            prepared_arceos_qemu_case("two", "test-arceos-std-two", &build_config),
            prepared_arceos_qemu_case("one/again", "test-arceos-std-one", &build_config),
        ];

        let groups = group_arceos_qemu_cases_by_build_identity(&cases);

        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].package, "test-arceos-std-one");
        assert_eq!(groups[0].cases.len(), 2);
        assert_eq!(groups[1].package, "test-arceos-std-two");
        assert_eq!(groups[1].cases.len(), 1);
    }

    fn prepared_arceos_qemu_case(
        name: &str,
        package: &str,
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
                    subcases: Vec::new(),
                },
                build_group: "std".to_string(),
                build_config_path: build_config_path.to_path_buf(),
                package: package.to_string(),
            },
            request: ResolvedBuildRequest {
                package: package.to_string(),
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
                package: package.to_string(),
                features: Vec::new(),
                log: None,
                extra_config: None,
                args: Vec::new(),
                pre_build_cmds: Vec::new(),
                post_build_cmds: Vec::new(),
                to_bin: false,
            },
            qemu: QemuConfig::default(),
        }
    }
}
