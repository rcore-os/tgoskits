use std::{
    collections::BTreeSet,
    fs,
    io::Write as _,
    path::{Path, PathBuf},
    process::{Command as StdCommand, Output, Stdio},
    sync::LazyLock,
};

use anyhow::{Context, bail};
use clap::{Args, Subcommand};
use ostool::{build::config::Cargo, run::qemu::QemuConfig};
use regex::Regex;

use super::{ArceOS, build, ensure_package_runtime_assets};
use crate::{
    context::{BuildCliArgs, ResolvedBuildRequest, SnapshotPersistence},
    support::process::ProcessExt,
    test::{case::TestQemuCase, qemu as qemu_test, suite as test_suite},
};

const ARCEOS_RUST_TEST_GROUP: &str = "rust";
const ARCEOS_C_TEST_GROUP: &str = "c";
const ARCEOS_TEST_SUITE_OS: &str = "arceos";
const C_TEST_BUILD_JOBS_ENV: &str = "AXBUILD_C_TEST_JOBS";

static ANSI_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\x1B(?:[@-Z\\-_]|\[[0-?]*[ -/]*[@-~])").expect("invalid ANSI stripping regex")
});

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
    dir: PathBuf,
    features: Vec<String>,
    invocations: Vec<CTestInvocation>,
}

/// One `test_one "..." "..."` entry from a C test `test_cmd`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CTestInvocation {
    make_vars: Vec<(String, String)>,
    expect_output: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PreparedCTestInvocation {
    label: String,
    make_args: Vec<String>,
    expect_output: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CTestCargoEnv {
    vars: Vec<(String, String)>,
}

struct CTestPrep {
    name: String,
    app_path: PathBuf,
    invocations: Vec<PreparedCTestInvocation>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CTestArtifactPaths {
    target_dir: PathBuf,
    out_dir: PathBuf,
    out_config: PathBuf,
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
                    Some(&target),
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

    let build_groups = qemu_test::prepare_case_build_groups(&prepared, |build_config_path| {
        let first_case = prepared
            .iter()
            .find(|case| case.case.build_config_path == build_config_path)
            .context("empty ArceOS Rust qemu build group")?;
        Ok((first_case.request.clone(), first_case.cargo.clone()))
    })?;
    let mut failed = Vec::new();
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
                    "failed to build ArceOS rust qemu test artifact for build group `{}` ({})",
                    build_group.group.build_group,
                    build_group.group.build_config_path.display()
                )
            })?;

        for case in &build_group.group.cases {
            completed += 1;
            let case_name = &case.case.case.name;
            println!("[{completed}/{total}] arceos rust qemu {case_name}");
            match run_rust_qemu_case(arceos, case)
                .await
                .with_context(|| format!("arceos rust qemu test failed for case `{case_name}`"))
            {
                Ok(()) => println!("ok: {}", case_name),
                Err(err) => {
                    eprintln!("failed: {}: {:#}", case_name, err);
                    failed.push(case_name.clone());
                }
            }
        }
    }

    qemu_test::finalize_qemu_test_run("arceos rust", "case", &failed)
}

async fn test_c_qemu(
    arceos: &mut ArceOS,
    target: &str,
    selected_case: Option<&str>,
) -> anyhow::Result<()> {
    run_c_qemu_tests_with_hooks(
        arceos.app.workspace_root(),
        target,
        selected_case,
        prepare_c_test_cargo_env,
        build_single_c_test,
        run_c_qemu_only,
    )
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

    let build_groups = qemu_test::prepare_case_build_groups(&prepared, |build_config_path| {
        let first_case = prepared
            .iter()
            .find(|case| case.case.build_config_path == build_config_path)
            .with_context(|| format!("empty ArceOS {group} qemu build group"))?;
        Ok((first_case.request.clone(), first_case.cargo.clone()))
    })?;
    let mut failed = Vec::new();
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
                    "failed to build ArceOS {group} qemu test artifact for build group `{}` ({})",
                    build_group.group.build_group,
                    build_group.group.build_config_path.display()
                )
            })?;

        for case in &build_group.group.cases {
            completed += 1;
            let case_name = &case.case.case.name;
            println!("[{completed}/{total}] {group_label} qemu {case_name}");
            match run_rust_qemu_case(arceos, case)
                .await
                .with_context(|| format!("{group_label} qemu test failed for case `{case_name}`"))
            {
                Ok(()) => println!("ok: {case_name}"),
                Err(err) => {
                    eprintln!("failed: {case_name}: {err:#}");
                    failed.push(case_name.clone());
                }
            }
        }
    }
    qemu_test::finalize_qemu_test_run(&group_label, "case", &failed)
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
        ensure_package_runtime_assets(&case.package)?;
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
    _target: Option<&str>,
    selected_case: Option<&str>,
) -> anyhow::Result<Option<String>> {
    let cases = c_qemu_case_names(arceos, selected_case)?;
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
    let tests = discover_c_tests(&arceos_c_test_dir(arceos))?;
    Ok(select_c_tests(tests, selected_case)?
        .into_iter()
        .map(|test| test.name)
        .collect())
}

fn c_qemu_listed_cases(
    arceos: &ArceOS,
    selected_case: Option<&str>,
) -> anyhow::Result<Vec<qemu_test::ListedQemuCase>> {
    let tests = discover_c_tests(&arceos_c_test_dir(arceos))?;
    Ok(select_c_tests(tests, selected_case)?
        .into_iter()
        .map(|test| qemu_test::ListedQemuCase {
            name: test.name,
            archs: c_test_archs(&test.dir),
        })
        .collect())
}

fn c_test_archs(dir: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut archs = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            if !path.is_dir() {
                return None;
            }
            let name = entry.file_name();
            name.to_str()
                .and_then(|name| name.strip_prefix("build_"))
                .map(|arch| arch.to_string())
        })
        .collect::<Vec<_>>();
    archs.sort();
    archs
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

fn select_c_tests(
    tests: Vec<CTestDef>,
    selected_case: Option<&str>,
) -> anyhow::Result<Vec<CTestDef>> {
    let Some(selected_case) = selected_case else {
        return Ok(tests);
    };
    let selected = tests
        .into_iter()
        .filter(|test| test.name == selected_case)
        .collect::<Vec<_>>();
    if selected.is_empty() {
        bail!("unknown ArceOS c qemu test case `{selected_case}`");
    }
    Ok(selected)
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

fn prepare_c_test_cargo_env(_workspace_root: &Path) -> CTestCargoEnv {
    CTestCargoEnv {
        vars: vec![
            (
                "CARGO_NET_GIT_FETCH_WITH_CLI".to_string(),
                "true".to_string(),
            ),
            (
                "CARGO_RESOLVER_INCOMPATIBLE_RUST_VERSIONS".to_string(),
                "allow".to_string(),
            ),
        ],
    }
}

/// Discover available C tests by checking which directories exist.
fn discover_c_tests(c_test_root: &Path) -> anyhow::Result<Vec<CTestDef>> {
    let mut tests = Vec::new();

    if !c_test_root.is_dir() {
        return Ok(tests);
    }

    discover_c_test_dirs(c_test_root, c_test_root, &mut tests)?;
    tests.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(tests)
}

fn discover_c_test_dirs(
    c_test_root: &Path,
    dir: &Path,
    tests: &mut Vec<CTestDef>,
) -> anyhow::Result<()> {
    let mut child_dirs = Vec::new();
    let mut has_c_source = false;

    for entry in
        fs::read_dir(dir).with_context(|| format!("failed to read C test dir {}", dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            child_dirs.push(path);
        } else if path.extension().is_some_and(|ext| ext == "c") {
            has_c_source = true;
        }
    }

    if has_c_source && has_c_test_marker(dir) {
        let relative = dir.strip_prefix(c_test_root).with_context(|| {
            format!(
                "failed to compute C test name for {} under {}",
                dir.display(),
                c_test_root.display()
            )
        })?;
        let name = relative
            .components()
            .map(|component| component.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        tests.push(CTestDef {
            name,
            dir: dir.to_path_buf(),
            features: load_features_txt(&dir.join("features.txt")),
            invocations: load_c_test_invocations(&dir.join("test_cmd"))?,
        });
    }

    child_dirs.sort();
    for child_dir in child_dirs {
        discover_c_test_dirs(c_test_root, &child_dir, tests)?;
    }

    Ok(())
}

fn has_c_test_marker(dir: &Path) -> bool {
    ["test_cmd", "features.txt", "axbuild.mk"]
        .iter()
        .any(|name| dir.join(name).is_file())
}

/// Load features from a `features.txt` file (one feature per line).
fn load_features_txt(path: &Path) -> Vec<String> {
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect()
}

fn load_c_test_invocations(path: &Path) -> anyhow::Result<Vec<CTestInvocation>> {
    if !path.exists() {
        return Ok(vec![CTestInvocation {
            make_vars: Vec::new(),
            expect_output: None,
        }]);
    }

    let test_one_regex = Regex::new(r#"^test_one\s+"([^"]*)"\s+"([^"]+)"\s*$"#)
        .expect("invalid C test command regex");
    let mut invocations = Vec::new();

    for (line_no, raw_line) in fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?
        .lines()
        .enumerate()
    {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') || line == "rm -f $APP/*.o" {
            continue;
        }

        let captures = test_one_regex.captures(line).ok_or_else(|| {
            anyhow::anyhow!("unsupported C test command at {}: {}", path.display(), line)
        })?;
        let make_vars = parse_c_test_make_vars(&captures[1]).with_context(|| {
            format!(
                "failed to parse make vars at {}:{}",
                path.display(),
                line_no + 1
            )
        })?;
        invocations.push(CTestInvocation {
            make_vars,
            expect_output: Some(PathBuf::from(&captures[2])),
        });
    }

    if invocations.is_empty() {
        invocations.push(CTestInvocation {
            make_vars: Vec::new(),
            expect_output: None,
        });
    }

    Ok(invocations)
}

fn parse_c_test_make_vars(input: &str) -> anyhow::Result<Vec<(String, String)>> {
    let mut vars = Vec::new();
    for assignment in input.split_whitespace() {
        let (key, value) = assignment
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("invalid make variable assignment `{assignment}`"))?;
        vars.push((key.to_string(), value.to_string()));
    }
    Ok(vars)
}

fn build_c_test_make_args(
    app_path: &Path,
    arch: &str,
    base_features: &[String],
    invocation: &CTestInvocation,
) -> Vec<String> {
    let makefile_features = build::makefile_features_from_env();
    build_c_test_make_args_with_makefile_features(
        app_path,
        arch,
        base_features,
        invocation,
        &makefile_features,
    )
}

fn build_c_test_make_args_with_makefile_features(
    app_path: &Path,
    arch: &str,
    base_features: &[String],
    invocation: &CTestInvocation,
    makefile_features: &[String],
) -> Vec<String> {
    let mut features = base_features.to_vec();
    let mut extra_vars = Vec::<(String, String)>::new();

    for feature in makefile_features {
        if !features.iter().any(|existing| existing == feature) {
            features.push(feature.clone());
        }
    }

    for (key, value) in &invocation.make_vars {
        if key == "FEATURES" {
            for feature in build::parse_makefile_features(value) {
                if !features.iter().any(|existing| existing == &feature) {
                    features.push(feature);
                }
            }
            continue;
        }

        match extra_vars.iter_mut().find(|(existing, _)| existing == key) {
            Some((_, existing_value)) => *existing_value = value.clone(),
            None => extra_vars.push((key.clone(), value.clone())),
        }
    }

    let mut args = vec![
        format!("A={}", app_path.display()),
        format!("ARCH={}", arch),
        "ACCEL=n".to_string(),
    ];
    if !features.is_empty() {
        args.push(format!("FEATURES={}", features.join(",")));
    }
    args.extend(
        extra_vars
            .into_iter()
            .map(|(key, value)| format!("{key}={value}")),
    );
    args
}

fn append_c_test_artifact_args(args: &mut Vec<String>, artifacts: &CTestArtifactPaths) {
    args.extend([
        format!("TARGET_DIR={}", artifacts.target_dir.display()),
        format!("OUT_DIR={}", artifacts.out_dir.display()),
        format!("OUT_CONFIG={}", artifacts.out_config.display()),
    ]);
}

fn runtime_output_regex(pattern: &str) -> anyhow::Result<Regex> {
    Regex::new(&translate_bre_to_regex(pattern))
        .with_context(|| format!("invalid expected-output regex `{pattern}`"))
}

fn translate_bre_to_regex(pattern: &str) -> String {
    let mut translated = String::new();
    let mut chars = pattern.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.next() {
                Some(next @ ('+' | '?' | '{' | '}' | '(' | ')' | '|')) => {
                    translated.push(next);
                }
                Some(next) => {
                    translated.push('\\');
                    translated.push(next);
                }
                None => translated.push('\\'),
            }
            continue;
        }

        if matches!(ch, '(' | ')' | '|' | '+' | '?' | '{' | '}') {
            translated.push('\\');
        }
        translated.push(ch);
    }

    translated
}

fn normalize_c_test_runtime_output(output: &Output) -> String {
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    ANSI_REGEX
        .replace_all(&combined.replace(['\r', '\0', '\u{0007}'], ""), "")
        .into_owned()
}

fn verify_c_test_runtime_output(output: &Output, expected_path: &Path) -> anyhow::Result<()> {
    let normalized = normalize_c_test_runtime_output(output);
    let actual_lines = normalized.lines().collect::<Vec<_>>();
    let expected = load_c_test_expected_output(expected_path)?;

    for (pattern, regex) in expected {
        if actual_lines.iter().any(|line| regex.is_match(line)) {
            continue;
        }

        let remaining = actual_lines
            .iter()
            .take(40)
            .copied()
            .collect::<Vec<_>>()
            .join("\n");
        bail!(
            "runtime output did not match `{pattern}` from {}. Captured output excerpt:\n{}",
            expected_path.display(),
            remaining
        );
    }

    Ok(())
}

fn load_c_test_expected_output(expected_path: &Path) -> anyhow::Result<Vec<(String, Regex)>> {
    let expected = fs::read_to_string(expected_path)
        .with_context(|| format!("failed to read {}", expected_path.display()))?;
    expected
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|pattern| Ok((pattern.to_string(), runtime_output_regex(pattern)?)))
        .collect()
}

fn c_test_invocation_label(invocation: &CTestInvocation) -> String {
    if invocation.make_vars.is_empty() {
        "default".to_string()
    } else {
        invocation
            .make_vars
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

fn prepare_c_test_invocations(
    app_path: &Path,
    arch: &str,
    base_features: &[String],
    invocations: &[CTestInvocation],
) -> Vec<PreparedCTestInvocation> {
    invocations
        .iter()
        .map(|invocation| PreparedCTestInvocation {
            label: c_test_invocation_label(invocation),
            make_args: build_c_test_make_args(app_path, arch, base_features, invocation),
            expect_output: invocation.expect_output.clone(),
        })
        .collect()
}

/// Two-phase C test runner:
///
/// Phase 1 – build all tests.  Builds default to serial because the ArceOS C
/// Makefiles share intermediate directories.  Build output is captured and
/// suppressed on success; it is printed verbatim only when a build fails,
/// keeping CI logs readable.
///
/// Phase 2 – run `make justrun` (QEMU) for each successfully-built test
/// **sequentially** so their output is never interleaved.  QEMU output is
/// streamed directly (captured + forwarded via `exec_capture`).
///
/// Both `build_fn` and `run_fn` are injectable for unit testing.
fn run_c_qemu_tests_with_hooks<PrepareCargoEnv, BuildFn, RunFn>(
    workspace_root: &Path,
    target: &str,
    selected_case: Option<&str>,
    prepare_cargo_env: PrepareCargoEnv,
    build_fn: BuildFn,
    mut run_fn: RunFn,
) -> anyhow::Result<()>
where
    PrepareCargoEnv: Fn(&Path) -> CTestCargoEnv,
    // `Fn + Send + Sync` so we can share an immutable reference across threads.
    BuildFn: Fn(&Path, &Path, &[String], &CTestCargoEnv) -> anyhow::Result<()> + Send + Sync,
    RunFn: FnMut(&Path, &Path, &PreparedCTestInvocation, &CTestCargoEnv) -> anyhow::Result<()>,
{
    let arch = crate::context::arch_for_target_checked(target)?;
    let arceos_dir = workspace_root.join("os/arceos");
    let c_test_root = arceos_test_group_dir(workspace_root, ARCEOS_C_TEST_GROUP);

    if !arceos_dir.join("Makefile").exists() {
        bail!(
            "arceos Makefile not found at {}, required for C test builds",
            arceos_dir.display()
        );
    }

    let c_tests = select_c_tests(discover_c_tests(&c_test_root)?, selected_case)?;
    if c_tests.is_empty() {
        println!("no C tests found in {}", c_test_root.display());
        return Ok(());
    }

    let cargo_env = prepare_cargo_env(workspace_root);

    // Prepare all test invocations, injecting per-invocation artifact paths so
    // multi-configuration tests do not overwrite each other's QEMU image.
    let mut preps = Vec::<CTestPrep>::new();
    let mut failed = Vec::new();

    for c_test in c_tests {
        let app_path = match c_test.dir.canonicalize() {
            Ok(path) => path,
            Err(err) => {
                eprintln!("failed: c/{}: cannot resolve path: {err:#}", c_test.name);
                failed.push(c_test.name.clone());
                continue;
            }
        };
        let mut invocations =
            prepare_c_test_invocations(&app_path, arch, &c_test.features, &c_test.invocations);
        let mut unique_build_args = Vec::<Vec<String>>::new();
        for inv in &mut invocations {
            let artifact_index = match unique_build_args
                .iter()
                .position(|args| args == &inv.make_args)
            {
                Some(index) => index,
                None => {
                    unique_build_args.push(inv.make_args.clone());
                    unique_build_args.len() - 1
                }
            };
            let artifacts = c_test_artifact_paths(workspace_root, &c_test.name, artifact_index);
            append_c_test_artifact_args(&mut inv.make_args, &artifacts);
        }
        preps.push(CTestPrep {
            name: c_test.name,
            app_path,
            invocations,
        });
    }

    println!(
        "running arceos C qemu tests for {} test(s) on target: {} (arch: {})",
        preps.len(),
        target,
        arch
    );

    // ---------------------------------------------------------------------------
    // Phase 1: Build all tests.
    //
    // Builds default to one job because ArceOS C Makefiles share intermediate
    // directories across tests.  If explicitly enabled, each worker runs
    // `make defconfig && make build` for one test.  Build output is captured;
    // only failures are printed after all workers finish.
    // ---------------------------------------------------------------------------
    let build_jobs = c_test_build_jobs(preps.len())?;
    if preps.len() > 1 {
        println!(
            "building {} C tests with up to {} job(s) (build output shown only on failure)…",
            preps.len(),
            build_jobs
        );

        if build_jobs < preps.len() {
            println!(
                "set {C_TEST_BUILD_JOBS_ENV}=N to experiment with ArceOS C test build \
                 parallelism; shared Makefile build directories may make values above 1 unsafe"
            );
        }
    }

    let build_errors = run_c_test_builds(&preps, &build_fn, &cargo_env, &arceos_dir, build_jobs);

    let failed_builds: BTreeSet<String> = build_errors.iter().map(|(n, _)| n.clone()).collect();
    for (name, err) in &build_errors {
        eprintln!("failed: c/{}: {:#}", name, err);
        failed.push(name.clone());
    }

    // ---------------------------------------------------------------------------
    // Phase 2: Run QEMU sequentially for tests that built successfully.
    // ---------------------------------------------------------------------------
    let total = preps.len();
    for (index, prep) in preps.iter().enumerate() {
        if failed_builds.contains(&prep.name) {
            continue;
        }

        println!("[{}/{}] arceos c qemu {}", index + 1, total, prep.name);

        let mut test_failed = false;
        for invocation in &prep.invocations {
            let result =
                run_fn(&arceos_dir, &prep.app_path, invocation, &cargo_env).with_context(|| {
                    format!("c test `{}` failed for `{}`", prep.name, invocation.label)
                });
            if let Err(err) = result {
                eprintln!("failed: c/{}: {:#}", prep.name, err);
                failed.push(prep.name.clone());
                test_failed = true;
                break;
            }
        }

        if !test_failed {
            println!("ok: c/{}", prep.name);
        }
    }

    qemu_test::finalize_qemu_test_run("arceos c", "test", &failed)
}

fn c_test_build_jobs(total: usize) -> anyhow::Result<usize> {
    // ArceOS C builds share Makefile-managed object directories such as
    // `ulib/axlibc/build_<arch>` and each app's `build_<arch>`.  Keep the
    // default deterministic; callers can still opt into parallelism explicitly.
    let default = 1;
    let Ok(value) = std::env::var(C_TEST_BUILD_JOBS_ENV) else {
        return Ok(default.max(1));
    };
    let trimmed = value.trim();
    let jobs = trimmed.parse::<usize>().with_context(|| {
        format!("invalid {C_TEST_BUILD_JOBS_ENV} value `{trimmed}`; expected positive integer")
    })?;
    if jobs == 0 {
        bail!("invalid {C_TEST_BUILD_JOBS_ENV} value `{trimmed}`; expected positive integer");
    }
    Ok(jobs.min(total.max(1)))
}

fn run_c_test_builds<BuildFn>(
    preps: &[CTestPrep],
    build_fn: &BuildFn,
    cargo_env: &CTestCargoEnv,
    arceos_dir: &Path,
    jobs: usize,
) -> Vec<(String, anyhow::Error)>
where
    BuildFn: Fn(&Path, &Path, &[String], &CTestCargoEnv) -> anyhow::Result<()> + Send + Sync,
{
    let chunk_size = preps.len().div_ceil(jobs.max(1));
    std::thread::scope(|s| {
        let handles: Vec<_> = preps
            .chunks(chunk_size.max(1))
            .map(|chunk| {
                s.spawn(move || {
                    chunk
                        .iter()
                        .filter_map(|prep| build_c_test_prep(prep, build_fn, arceos_dir, cargo_env))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| {
                h.join().unwrap_or_else(|panic| {
                    vec![(
                        "build-worker".to_string(),
                        anyhow::anyhow!("build thread panicked: {}", panic_payload(panic)),
                    )]
                })
            })
            .collect()
    })
}

fn panic_payload(panic: Box<dyn std::any::Any + Send>) -> String {
    panic
        .downcast_ref::<&str>()
        .map(|value| (*value).to_string())
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic payload".to_string())
}

fn build_c_test_prep<BuildFn>(
    prep: &CTestPrep,
    build_fn: &BuildFn,
    arceos_dir: &Path,
    cargo_env: &CTestCargoEnv,
) -> Option<(String, anyhow::Error)>
where
    BuildFn: Fn(&Path, &Path, &[String], &CTestCargoEnv) -> anyhow::Result<()> + Send + Sync,
{
    let mut built_args = Vec::<Vec<String>>::new();
    for inv in &prep.invocations {
        let already_built = built_args
            .iter()
            .any(|a| a.as_slice() == inv.make_args.as_slice());
        if already_built {
            continue;
        }
        if let Err(err) = build_fn(arceos_dir, &prep.app_path, &inv.make_args, cargo_env) {
            return Some((
                prep.name.clone(),
                err.context(format!("c test `{}` build failed", prep.name)),
            ));
        }
        built_args.push(inv.make_args.clone());
    }
    None
}

/// Returns isolated artifact paths for a single C test invocation.
///
/// `make build` writes the final image to `OUT_DIR` and the generated config
/// to `OUT_CONFIG`.  Multi-invocation tests such as `helloworld` build several
/// configurations for the same app, so every invocation needs its own outputs
/// for the later `make justrun` phase to execute the matching image.
fn c_test_artifact_paths(
    workspace_root: &Path,
    test_name: &str,
    invocation_index: usize,
) -> CTestArtifactPaths {
    let dir_name = format!(
        "arceos-c-{}-{}",
        test_name.replace('/', "-"),
        invocation_index
    );
    let root = crate::context::axbuild_tmp_dir(workspace_root)
        .join("arceos-c")
        .join(dir_name);
    CTestArtifactPaths {
        target_dir: root.join("cargo"),
        out_dir: root.join("out"),
        out_config: root.join("axconfig.toml"),
    }
}

/// Build phase of a single C test: runs `make defconfig && make build`.
///
/// Build output is **captured and suppressed** on success to keep parallel
/// build logs readable.  On failure the captured output is flushed to stderr
/// before the error is returned.
fn build_single_c_test(
    arceos_dir: &Path,
    _app_path: &Path,
    make_args: &[String],
    cargo_env: &CTestCargoEnv,
) -> anyhow::Result<()> {
    ensure_c_test_artifact_dirs(make_args)?;

    for target in ["defconfig", "build"] {
        let mut command = StdCommand::new("make");
        command.current_dir(arceos_dir).args(make_args).arg(target);
        apply_c_test_cargo_env(&mut command, cargo_env);
        // Capture stdout/stderr so parallel builds don't interleave output.
        let output = command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .with_context(|| format!("failed to spawn `make {target}`"))?;
        if !output.status.success() {
            // Print captured output so the error context is visible.
            let _ = std::io::stderr().write_all(&output.stderr);
            let _ = std::io::stdout().write_all(&output.stdout);
            bail!("`make {}` exited with {}", target, output.status);
        }
    }
    Ok(())
}

fn ensure_c_test_artifact_dirs(make_args: &[String]) -> anyhow::Result<()> {
    for key in ["TARGET_DIR", "OUT_DIR"] {
        if let Some(path) = make_arg_value(make_args, key) {
            fs::create_dir_all(path)
                .with_context(|| format!("failed to create {key} directory {}", path.display()))?;
        }
    }

    if let Some(out_config) = make_arg_value(make_args, "OUT_CONFIG")
        && let Some(parent) = out_config.parent()
    {
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create OUT_CONFIG parent directory {}",
                parent.display()
            )
        })?;
    }

    Ok(())
}

fn make_arg_value<'a>(args: &'a [String], key: &str) -> Option<&'a Path> {
    let prefix = format!("{key}=");
    args.iter()
        .find_map(|arg| arg.strip_prefix(prefix.as_str()))
        .map(Path::new)
}

/// QEMU phase of a single C test: runs `make justrun` and verifies output.
fn run_c_qemu_only(
    arceos_dir: &Path,
    app_path: &Path,
    invocation: &PreparedCTestInvocation,
    cargo_env: &CTestCargoEnv,
) -> anyhow::Result<()> {
    let mut command = StdCommand::new("make");
    command
        .current_dir(arceos_dir)
        .args(&invocation.make_args)
        .arg("justrun");
    apply_c_test_cargo_env(&mut command, cargo_env);
    let output = command.exec_capture()?;

    if let Some(expect_output) = &invocation.expect_output {
        verify_c_test_runtime_output(&output, &app_path.join(expect_output))?;
    }

    Ok(())
}

fn apply_c_test_cargo_env(command: &mut StdCommand, cargo_env: &CTestCargoEnv) {
    for (key, value) in &cargo_env.vars {
        command.env(key, value);
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
    use std::{
        os::unix::process::ExitStatusExt,
        sync::{Arc, Mutex},
    };

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

    #[test]
    fn load_features_txt_parses_correctly() {
        let dir = std::env::temp_dir().join("axbuild_test_features");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("features.txt"), "alloc\npaging\nnet\n").unwrap();

        let features = load_features_txt(&dir.join("features.txt"));
        assert_eq!(features, vec!["alloc", "paging", "net"]);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn load_features_txt_handles_missing_file() {
        let features = load_features_txt(Path::new("/nonexistent/features.txt"));
        assert!(features.is_empty());
    }

    #[test]
    fn discover_c_tests_finds_valid_tests() {
        let dir = std::env::temp_dir().join("axbuild_test_c");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("helloworld")).unwrap();
        std::fs::write(dir.join("helloworld/main.c"), "int main() { return 0; }\n").unwrap();
        std::fs::write(
            dir.join("helloworld/test_cmd"),
            "test_one \"LOG=info\" \"expect_info.out\"\n",
        )
        .unwrap();
        std::fs::create_dir_all(dir.join("pthread/basic")).unwrap();
        std::fs::write(
            dir.join("pthread/basic/main.c"),
            "int main() { return 0; }\n",
        )
        .unwrap();
        std::fs::write(dir.join("pthread/basic/features.txt"), "pthread\n").unwrap();
        std::fs::create_dir_all(dir.join("helpers")).unwrap();
        std::fs::write(dir.join("helpers/helper.c"), "void helper(void) {}\n").unwrap();
        std::fs::create_dir_all(dir.join("empty")).unwrap();

        let tests = discover_c_tests(&dir).unwrap();
        // Directories without C sources or test markers are ignored.
        assert_eq!(
            tests
                .iter()
                .map(|test| test.name.as_str())
                .collect::<Vec<_>>(),
            vec!["helloworld", "pthread/basic"]
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn load_c_test_invocations_parses_test_cmd() {
        let dir = tempdir().unwrap();
        let test_cmd = dir.path().join("test_cmd");
        std::fs::write(
            &test_cmd,
            "test_one \"SMP=4 LOG=info FEATURES=sched-rr\" \"expect.out\"\nrm -f $APP/*.o\n",
        )
        .unwrap();

        let invocations = load_c_test_invocations(&test_cmd).unwrap();
        assert_eq!(invocations.len(), 1);
        assert_eq!(
            invocations[0].make_vars,
            vec![
                ("SMP".to_string(), "4".to_string()),
                ("LOG".to_string(), "info".to_string()),
                ("FEATURES".to_string(), "sched-rr".to_string())
            ]
        );
        assert_eq!(
            invocations[0].expect_output,
            Some(PathBuf::from("expect.out"))
        );
    }

    #[test]
    fn build_c_test_make_args_merges_makefile_features_from_env_and_invocation() {
        let invocation = CTestInvocation {
            make_vars: vec![
                ("FEATURES".to_string(), "sched-rr".to_string()),
                ("LOG".to_string(), "info".to_string()),
            ],
            expect_output: None,
        };

        let args = build_c_test_make_args_with_makefile_features(
            Path::new("/tmp/case"),
            "x86_64",
            &[String::from("net")],
            &invocation,
            &[String::from("lockdep"), String::from("net")],
        );

        assert!(args.contains(&"FEATURES=net,lockdep,sched-rr".to_string()));
        assert!(args.contains(&"LOG=info".to_string()));
    }

    #[test]
    fn prepare_c_test_cargo_env_uses_env_vars_only() {
        let env = prepare_c_test_cargo_env(Path::new("/repo"));

        assert_eq!(
            env.vars,
            vec![
                (
                    "CARGO_NET_GIT_FETCH_WITH_CLI".to_string(),
                    "true".to_string()
                ),
                (
                    "CARGO_RESOLVER_INCOMPATIBLE_RUST_VERSIONS".to_string(),
                    "allow".to_string()
                ),
            ]
        );
    }

    #[test]
    fn translate_bre_to_regex_handles_bre_quantifiers_and_literals() {
        let translated =
            translate_bre_to_regex(r"task 15 actually sleep 5\.[0-9]\+ seconds (2) ...");
        let regex = Regex::new(&translated).unwrap();
        assert!(regex.is_match("task 15 actually sleep 5.009334 seconds (2) ..."));
    }

    #[test]
    fn verify_c_test_runtime_output_matches_expected_lines_in_order() {
        let dir = tempdir().unwrap();
        let expected = dir.path().join("expect.out");
        std::fs::write(
            &expected,
            "Hello, C app!\nvalue = [0-9]\\+\nShutting down...\n",
        )
        .unwrap();
        let output = Output {
            status: std::process::ExitStatus::from_raw(0),
            stdout: b"noise\nHello, C app!\nvalue = 42\nShutting down...\n".to_vec(),
            stderr: Vec::new(),
        };

        verify_c_test_runtime_output(&output, &expected).unwrap();
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
    fn run_c_qemu_tests_with_hooks_prepares_cargo_env_once_before_running_tests() {
        let dir = tempdir().unwrap();
        let workspace_root = dir.path();
        let arceos_dir = workspace_root.join("os/arceos");
        let c_root = arceos_test_group_dir(workspace_root, ARCEOS_C_TEST_GROUP);

        std::fs::create_dir_all(&arceos_dir).unwrap();
        std::fs::create_dir_all(c_root.join("helloworld")).unwrap();
        std::fs::create_dir_all(c_root.join("memtest")).unwrap();
        std::fs::write(arceos_dir.join("Makefile"), "run:\n\t@true\n").unwrap();
        std::fs::write(
            c_root.join("helloworld/main.c"),
            "int main(void) { return 0; }\n",
        )
        .unwrap();
        std::fs::write(c_root.join("helloworld/test_cmd"), "").unwrap();
        std::fs::write(
            c_root.join("memtest/main.c"),
            "int main(void) { return 0; }\n",
        )
        .unwrap();
        std::fs::write(c_root.join("memtest/test_cmd"), "").unwrap();

        let events = Arc::new(Mutex::new(Vec::new()));

        let prepare_env_events = events.clone();
        let build_events = events.clone();
        let run_events = events.clone();
        run_c_qemu_tests_with_hooks(
            workspace_root,
            "x86_64-unknown-none",
            None,
            move |root| {
                prepare_env_events
                    .lock()
                    .unwrap()
                    .push(format!("prepare_env:{}", root.display()));
                prepare_c_test_cargo_env(root)
            },
            move |_arceos_dir, app_path, _make_args, _cargo_env| {
                build_events.lock().unwrap().push(format!(
                    "build:{}",
                    app_path.file_name().unwrap().to_string_lossy()
                ));
                Ok(())
            },
            move |_arceos_dir, app_path, _invocation, _cargo_env| {
                run_events.lock().unwrap().push(format!(
                    "run:{}",
                    app_path.file_name().unwrap().to_string_lossy()
                ));
                Ok(())
            },
        )
        .unwrap();

        let events = events.lock().unwrap();
        assert_eq!(
            events[0],
            format!("prepare_env:{}", workspace_root.display())
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| event.starts_with("prepare_env:"))
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| event.starts_with("run:"))
                .count(),
            2
        );
    }

    #[test]
    fn run_c_qemu_tests_with_hooks_reuses_duplicate_build_args() {
        let dir = tempdir().unwrap();
        let workspace_root = dir.path();
        let arceos_dir = workspace_root.join("os/arceos");
        let c_root = arceos_test_group_dir(workspace_root, ARCEOS_C_TEST_GROUP);

        std::fs::create_dir_all(&arceos_dir).unwrap();
        std::fs::create_dir_all(c_root.join("helloworld")).unwrap();
        std::fs::write(arceos_dir.join("Makefile"), "run:\n\t@true\n").unwrap();
        std::fs::write(
            c_root.join("helloworld/main.c"),
            "int main(void) { return 0; }\n",
        )
        .unwrap();
        std::fs::write(
            c_root.join("helloworld/test_cmd"),
            "test_one \"LOG=info\" \"expect_info.out\"\ntest_one \"LOG=info\" \
             \"expect_info_again.out\"\n",
        )
        .unwrap();

        let build_events = Arc::new(Mutex::new(Vec::new()));
        let run_events = Arc::new(Mutex::new(Vec::new()));
        let build_events_clone = build_events.clone();
        let run_events_clone = run_events.clone();
        run_c_qemu_tests_with_hooks(
            workspace_root,
            "x86_64-unknown-none",
            None,
            prepare_c_test_cargo_env,
            move |_arceos_dir, _app_path, make_args, _cargo_env| {
                build_events_clone
                    .lock()
                    .unwrap()
                    .push(format!("build_args={}", make_args.len()));
                Ok(())
            },
            move |_arceos_dir, _app_path, invocation, _cargo_env| {
                run_events_clone
                    .lock()
                    .unwrap()
                    .push(format!("label={}", invocation.label));
                Ok(())
            },
        )
        .unwrap();

        // The two invocations share identical make_args (both "LOG=info"), so
        // the build phase must only call build_fn once (deduplication).
        let builds = build_events.lock().unwrap();
        assert_eq!(
            builds.len(),
            1,
            "build should be deduplicated to a single call"
        );

        // Both invocations must be executed by the QEMU phase.
        let runs = run_events.lock().unwrap();
        assert_eq!(runs.len(), 2, "both invocations should be run");
    }

    #[test]
    fn run_c_qemu_tests_with_hooks_isolates_distinct_invocation_artifacts() {
        let dir = tempdir().unwrap();
        let workspace_root = dir.path();
        let arceos_dir = workspace_root.join("os/arceos");
        let c_root = arceos_test_group_dir(workspace_root, ARCEOS_C_TEST_GROUP);

        std::fs::create_dir_all(&arceos_dir).unwrap();
        std::fs::create_dir_all(c_root.join("helloworld")).unwrap();
        std::fs::write(arceos_dir.join("Makefile"), "run:\n\t@true\n").unwrap();
        std::fs::write(
            c_root.join("helloworld/main.c"),
            "int main(void) { return 0; }\n",
        )
        .unwrap();
        std::fs::write(
            c_root.join("helloworld/test_cmd"),
            "test_one \"LOG=info\" \"expect_info.out\"\ntest_one \"SMP=4 LOG=info\" \
             \"expect_info_smp4.out\"\ntest_one \"LOG=info\" \"expect_info_again.out\"\n",
        )
        .unwrap();

        let run_args = Arc::new(Mutex::new(Vec::<Vec<String>>::new()));
        let run_args_clone = run_args.clone();
        run_c_qemu_tests_with_hooks(
            workspace_root,
            "x86_64-unknown-none",
            None,
            prepare_c_test_cargo_env,
            move |_arceos_dir, _app_path, _make_args, _cargo_env| Ok(()),
            move |_arceos_dir, _app_path, invocation, _cargo_env| {
                run_args_clone
                    .lock()
                    .unwrap()
                    .push(invocation.make_args.clone());
                Ok(())
            },
        )
        .unwrap();

        let run_args = run_args.lock().unwrap();
        assert_eq!(run_args.len(), 3);
        let first_target_dir = make_arg_value(&run_args[0], "TARGET_DIR").unwrap();
        let first_out_dir = make_arg_value(&run_args[0], "OUT_DIR").unwrap();
        let first_out_config = make_arg_value(&run_args[0], "OUT_CONFIG").unwrap();
        let second_target_dir = make_arg_value(&run_args[1], "TARGET_DIR").unwrap();
        let second_out_dir = make_arg_value(&run_args[1], "OUT_DIR").unwrap();
        let second_out_config = make_arg_value(&run_args[1], "OUT_CONFIG").unwrap();
        let third_target_dir = make_arg_value(&run_args[2], "TARGET_DIR").unwrap();
        let third_out_dir = make_arg_value(&run_args[2], "OUT_DIR").unwrap();
        let third_out_config = make_arg_value(&run_args[2], "OUT_CONFIG").unwrap();

        assert_ne!(first_target_dir, second_target_dir);
        assert_ne!(first_out_dir, second_out_dir);
        assert_ne!(first_out_config, second_out_config);
        assert_eq!(first_target_dir, third_target_dir);
        assert_eq!(first_out_dir, third_out_dir);
        assert_eq!(first_out_config, third_out_config);
    }

    #[test]
    fn ensure_c_test_artifact_dirs_creates_output_parents() {
        let dir = tempdir().unwrap();
        let target_dir = dir.path().join("target/cargo");
        let out_dir = dir.path().join("target/out");
        let out_config = dir.path().join("target/configs/axconfig.toml");

        ensure_c_test_artifact_dirs(&[
            format!("TARGET_DIR={}", target_dir.display()),
            format!("OUT_DIR={}", out_dir.display()),
            format!("OUT_CONFIG={}", out_config.display()),
        ])
        .unwrap();

        assert!(target_dir.is_dir());
        assert!(out_dir.is_dir());
        assert!(out_config.parent().unwrap().is_dir());
    }

    fn make_arg_value<'a>(args: &'a [String], key: &str) -> Option<&'a str> {
        let prefix = format!("{key}=");
        args.iter()
            .find_map(|arg| arg.strip_prefix(prefix.as_str()))
    }

    #[test]
    fn c_test_build_jobs_rejects_invalid_env_value() {
        unsafe {
            std::env::set_var(C_TEST_BUILD_JOBS_ENV, "0");
        }
        let err = c_test_build_jobs(2).unwrap_err().to_string();
        unsafe {
            std::env::remove_var(C_TEST_BUILD_JOBS_ENV);
        }

        assert!(err.contains(C_TEST_BUILD_JOBS_ENV));
        assert!(err.contains("positive integer"));

        assert_eq!(c_test_build_jobs(8).unwrap(), 1);
    }
}
