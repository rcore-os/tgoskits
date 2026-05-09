use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command as StdCommand, Output},
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

pub(crate) const TEST_TARGETS: &[&str] = &[
    "x86_64-unknown-none",
    "riscv64gc-unknown-none-elf",
    "aarch64-unknown-none-softfloat",
    "loongarch64-unknown-none-softfloat",
];
pub(crate) const TEST_ARCHES: &[&str] = &["x86_64", "riscv64", "aarch64", "loongarch64"];

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

#[derive(Debug, Clone, PartialEq, Eq)]
struct PreparedArceosRustQemuCase {
    case: ArceosRustQemuCase,
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
        TEST_ARCHES,
        TEST_TARGETS,
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
    let mut failed = Vec::new();
    let cases = discover_rust_qemu_cases(arceos, arch, target, selected_case)?;
    println!(
        "running arceos rust qemu tests for arch: {} (target: {}, cases: {})",
        arch,
        target,
        cases.len()
    );

    let prepared = prepare_rust_qemu_cases(arceos, target, cases).await?;
    let total = prepared.len();
    let mut completed = 0;
    for group in qemu_test::group_cases_by_build_config(&prepared) {
        let package = group
            .cases
            .first()
            .map(|case| case.case.package.as_str())
            .context("empty ArceOS Rust qemu build group")?;
        let request = arceos.prepare_request(
            test_build_args(package, target, group.build_config_path),
            None,
            None,
            SnapshotPersistence::Discard,
        )?;
        let cargo = build::load_cargo_config(&request)?;
        arceos
            .app
            .build(cargo.clone(), request.build_info_path.clone())
            .await
            .with_context(|| {
                format!(
                    "failed to build ArceOS rust qemu test artifact for build group `{}` ({})",
                    group.build_group,
                    group.build_config_path.display()
                )
            })?;

        for case in group.cases {
            completed += 1;
            let case_name = &case.case.case.name;
            println!("[{completed}/{total}] arceos rust qemu {case_name}");
            match run_rust_qemu_case(arceos, &request, &cargo, case)
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
        run_single_c_qemu_test,
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
    let mut completed = 0;
    let mut failed = Vec::new();
    for build_group in qemu_test::group_cases_by_build_config(&prepared) {
        let package = build_group
            .cases
            .first()
            .map(|case| case.case.package.as_str())
            .with_context(|| format!("empty ArceOS {group} qemu build group"))?;
        let request = arceos.prepare_request(
            test_build_args(package, target, build_group.build_config_path),
            None,
            None,
            SnapshotPersistence::Discard,
        )?;
        let cargo = build::load_cargo_config(&request)?;
        arceos
            .app
            .build(cargo.clone(), request.build_info_path.clone())
            .await
            .with_context(|| {
                format!(
                    "failed to build ArceOS {group} qemu test artifact for build group `{}` ({})",
                    build_group.build_group,
                    build_group.build_config_path.display()
                )
            })?;
        for case in build_group.cases {
            completed += 1;
            let case_name = &case.case.case.name;
            println!("[{completed}/{total}] {group_label} qemu {case_name}");
            match run_rust_qemu_case(arceos, &request, &cargo, case)
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
        prepared.push(PreparedArceosRustQemuCase { case, qemu });
    }
    Ok(prepared)
}

async fn run_rust_qemu_case(
    arceos: &mut ArceOS,
    _request: &ResolvedBuildRequest,
    cargo: &Cargo,
    case: &PreparedArceosRustQemuCase,
) -> anyhow::Result<()> {
    arceos.app.run_qemu(cargo, case.qemu.clone()).await
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
        None => qemu_test::discover_all_qemu_cases(&dir, selected_case, "ArceOS", group)?,
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
                .filter(|v| !v.is_empty()),
            ARCEOS_C_TEST_GROUP => c_qemu_listed_cases(arceos, selected_case)
                .ok()
                .filter(|v| !v.is_empty()),
            _ => {
                let dir = arceos_test_group_dir(arceos.app.workspace_root(), &group);
                qemu_test::discover_all_qemu_cases_with_archs(&dir, selected_case, "ArceOS", &group)
                    .ok()
                    .filter(|v| !v.is_empty())
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

fn rust_qemu_listed_cases(
    arceos: &ArceOS,
    selected_case: Option<&str>,
) -> anyhow::Result<Vec<qemu_test::ListedQemuCase>> {
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
        ),
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
    let ansi_regex =
        Regex::new(r"\x1B(?:[@-Z\\-_]|\[[0-?]*[ -/]*[@-~])").expect("invalid ANSI stripping regex");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    ansi_regex
        .replace_all(&combined.replace(['\r', '\0', '\u{0007}'], ""), "")
        .into_owned()
}

fn verify_c_test_runtime_output(output: &Output, expected_path: &Path) -> anyhow::Result<()> {
    let normalized = normalize_c_test_runtime_output(output);
    let actual_lines = normalized.lines().collect::<Vec<_>>();
    let expected = fs::read_to_string(expected_path)
        .with_context(|| format!("failed to read {}", expected_path.display()))?;

    for pattern in expected
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        let regex = runtime_output_regex(pattern)?;
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

fn c_test_build_key(invocation: &PreparedCTestInvocation) -> &[String] {
    &invocation.make_args
}

/// Extract architecture short name from a target triple (e.g. "x86_64-unknown-none" -> "x86_64").
fn arch_from_target(target: &str) -> &str {
    if target.starts_with("x86_64") {
        "x86_64"
    } else if target.starts_with("aarch64") {
        "aarch64"
    } else if target.starts_with("riscv64") {
        "riscv64"
    } else if target.starts_with("loongarch64") {
        "loongarch64"
    } else {
        "unknown"
    }
}

fn run_c_qemu_tests_with_hooks<PrepareCargoEnv, RunTest>(
    workspace_root: &Path,
    target: &str,
    selected_case: Option<&str>,
    mut prepare_cargo_env: PrepareCargoEnv,
    mut run_test: RunTest,
) -> anyhow::Result<()>
where
    PrepareCargoEnv: FnMut(&Path) -> CTestCargoEnv,
    RunTest: FnMut(
        &Path,
        &Path,
        &[String],
        &PreparedCTestInvocation,
        &CTestCargoEnv,
    ) -> anyhow::Result<()>,
{
    let arch = arch_from_target(target);
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

    let mut failed = Vec::new();
    println!(
        "running arceos C qemu tests for {} test(s) on target: {} (arch: {})",
        c_tests.len(),
        target,
        arch
    );

    for (index, c_test) in c_tests.iter().enumerate() {
        println!(
            "[{}/{}] arceos c qemu {}",
            index + 1,
            c_tests.len(),
            c_test.name
        );

        let app_path = match c_test.dir.canonicalize() {
            Ok(path) => path,
            Err(err) => {
                eprintln!("failed: c/{}: cannot resolve path: {err:#}", c_test.name);
                failed.push(c_test.name.clone());
                continue;
            }
        };

        let invocations =
            prepare_c_test_invocations(&app_path, arch, &c_test.features, &c_test.invocations);
        let mut built_make_args = Vec::<Vec<String>>::new();

        let mut test_failed = false;
        for invocation in &invocations {
            let build_needed = !built_make_args
                .iter()
                .any(|make_args| make_args.as_slice() == c_test_build_key(invocation));
            let build_args = if build_needed {
                invocation.make_args.as_slice()
            } else {
                &[]
            };
            let result = run_test(&arceos_dir, &app_path, build_args, invocation, &cargo_env)
                .with_context(|| {
                    format!("c test `{}` failed for `{}`", c_test.name, invocation.label)
                });
            if let Err(err) = result {
                eprintln!("failed: c/{}: {:#}", c_test.name, err);
                failed.push(c_test.name.clone());
                test_failed = true;
                break;
            }
            if build_needed {
                built_make_args.push(invocation.make_args.clone());
            }
        }

        if !test_failed {
            println!("ok: c/{}", c_test.name);
        }
    }

    qemu_test::finalize_qemu_test_run("arceos c", "test", &failed)
}

fn run_single_c_qemu_test(
    arceos_dir: &Path,
    app_path: &Path,
    build_make_args: &[String],
    invocation: &PreparedCTestInvocation,
    cargo_env: &CTestCargoEnv,
) -> anyhow::Result<()> {
    if !build_make_args.is_empty() {
        let mut command = StdCommand::new("make");
        command
            .current_dir(arceos_dir)
            .args(build_make_args)
            .arg("defconfig");
        apply_c_test_cargo_env(&mut command, cargo_env);
        command.exec()?;

        let mut command = StdCommand::new("make");
        command
            .current_dir(arceos_dir)
            .args(build_make_args)
            .arg("build");
        apply_c_test_cargo_env(&mut command, cargo_env);
        command.exec()?;
    }

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
        TEST_ARCHES,
        TEST_TARGETS,
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
    fn arch_from_target_extracts_correct_arch() {
        assert_eq!(arch_from_target("x86_64-unknown-none"), "x86_64");
        assert_eq!(
            arch_from_target("aarch64-unknown-none-softfloat"),
            "aarch64"
        );
        assert_eq!(arch_from_target("riscv64gc-unknown-none-elf"), "riscv64");
        assert_eq!(
            arch_from_target("loongarch64-unknown-none-softfloat"),
            "loongarch64"
        );
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
            move |_arceos_dir, app_path, _build_args, _invocation, _cargo_env| {
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

        let events = Arc::new(Mutex::new(Vec::new()));
        let run_events = events.clone();
        run_c_qemu_tests_with_hooks(
            workspace_root,
            "x86_64-unknown-none",
            None,
            prepare_c_test_cargo_env,
            move |_arceos_dir, _app_path, build_args, invocation, _cargo_env| {
                run_events.lock().unwrap().push(format!(
                    "build_args={} label={}",
                    build_args.len(),
                    invocation.label
                ));
                Ok(())
            },
        )
        .unwrap();

        let events = events.lock().unwrap();
        assert_eq!(events.len(), 2);
        assert!(events[0].starts_with("build_args=4 "));
        assert!(events[1].starts_with("build_args=0 "));
    }
}
