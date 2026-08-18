use std::{
    collections::{BTreeSet, HashSet},
    fs,
    io::{self, Write},
    path::Path,
    process::Command,
};

use anyhow::{Context, bail};
use cargo_metadata::{Metadata, Package};
use clap::Args;

use crate::support::{git::IncrementalPackageSelection, process::run_cargo_status};

const STD_CRATES_CSV: &str = "scripts/test/std_crates.csv";
const TASK_INITIALIZATION_FILTER: &str = "task_initialization_precedes_scheduling";

#[derive(Args, Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct StdTestArgs {
    /// Run std tests only for workspace packages affected since the git ref
    #[arg(long, value_name = "REF")]
    pub(crate) since: Option<String>,
}

#[derive(Clone, Copy, Debug)]
struct PackageFeatureProfile {
    name: &'static str,
    features: &'static [&'static str],
    name_filter: Option<&'static str>,
    expected_tests: &'static [&'static str],
}

const AX_TASK_FEATURE_PROFILES: &[PackageFeatureProfile] = &[
    PackageFeatureProfile {
        name: "host-test+multitask+irq-pure",
        features: &["host-test", "multitask", "irq"],
        name_filter: Some("std_tests::"),
        expected_tests: &[
            "api::std_tests::axtask_api_constants_hold",
            "api::std_tests::axtask_api_scheduler_name_hold",
            "api::std_tests::axtask_api_task_registry_functions_exist_hold",
            "api::std_tests::axtask_api_type_aliases_hold",
        ],
    },
    PackageFeatureProfile {
        name: "host-test+multitask-task-initialization",
        features: &["host-test", "multitask"],
        name_filter: Some(TASK_INITIALIZATION_FILTER),
        expected_tests: &["api::tests::task_initialization_precedes_scheduling"],
    },
];

const AX_DRIVER_FEATURE_PROFILES: &[PackageFeatureProfile] = &[PackageFeatureProfile {
    name: "host-test+rtc+starfive-jh7110-dwmmc",
    features: &["host-test", "rtc", "starfive-jh7110-dwmmc"],
    name_filter: None,
    expected_tests: &[],
}];

const HOST_TEST_FEATURE_PROFILES: &[PackageFeatureProfile] = &[PackageFeatureProfile {
    name: "host-test",
    features: &["host-test"],
    name_filter: None,
    expected_tests: &[],
}];

const ALLOC_FEATURE_PROFILES: &[PackageFeatureProfile] = &[PackageFeatureProfile {
    name: "alloc",
    features: &["alloc"],
    name_filter: None,
    expected_tests: &[],
}];

const FS_FEATURE_PROFILES: &[PackageFeatureProfile] = &[PackageFeatureProfile {
    name: "fs",
    features: &["fs"],
    name_filter: None,
    expected_tests: &[],
}];

const STARRY_KERNEL_FEATURE_PROFILES: &[PackageFeatureProfile] = &[PackageFeatureProfile {
    name: "std-tests-only",
    features: &[],
    name_filter: Some("std_tests::"),
    expected_tests: &[],
}];

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum CargoTestAction {
    List,
    Run,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct CargoTestInvocation {
    package: String,
    features: Vec<String>,
    name_filter: Option<String>,
    action: CargoTestAction,
}

impl CargoTestInvocation {
    fn default_for(package: &str) -> Self {
        Self {
            package: package.to_owned(),
            features: Vec::new(),
            name_filter: None,
            action: CargoTestAction::Run,
        }
    }

    fn for_profile(
        package: &str,
        profile: &PackageFeatureProfile,
        action: CargoTestAction,
    ) -> Self {
        Self {
            package: package.to_owned(),
            features: profile
                .features
                .iter()
                .map(|feature| (*feature).to_owned())
                .collect(),
            name_filter: profile.name_filter.map(str::to_owned),
            action,
        }
    }

    fn args(&self) -> Vec<String> {
        let mut args = vec!["test".into(), "-p".into(), self.package.clone()];
        if !self.features.is_empty() {
            args.push("--features".into());
            args.push(self.features.join(","));
        }
        if let Some(name_filter) = &self.name_filter {
            args.push(name_filter.clone());
        }
        if self.action == CargoTestAction::List {
            args.extend(["--".into(), "--list".into()]);
        }
        args
    }
}

#[derive(Clone, Debug)]
struct CargoRunOutput {
    success: bool,
    stdout: String,
}

pub(crate) fn run_std_test_command(args: &StdTestArgs) -> anyhow::Result<()> {
    let workspace_manifest = crate::context::workspace_manifest_path()?;
    let metadata = if args.since.is_some() {
        crate::context::workspace_metadata_root_manifest_with_deps(&workspace_manifest)
    } else {
        crate::context::workspace_metadata_root_manifest(&workspace_manifest)
    }
    .context("failed to load cargo metadata")?;
    let workspace_root = metadata.workspace_root.clone().into_std_path_buf();
    let known_packages = workspace_package_names(&metadata);
    let csv_path = workspace_root.join(STD_CRATES_CSV);
    let all_packages = load_std_crates(&csv_path, &known_packages)?;
    let packages = match args.since.as_deref() {
        None => all_packages,
        Some(since) => {
            let workspace_packages = workspace_packages(&metadata);
            let selection = crate::support::git::select_incremental_packages(
                &workspace_root,
                &metadata,
                &workspace_packages,
                since,
            )
            .unwrap_or_else(|error| IncrementalPackageSelection::Full {
                reason: format!("incremental std test selection failed: {error:#}"),
            });
            match &selection {
                IncrementalPackageSelection::Packages { changed, affected } => println!(
                    "incremental std tests since {since}: changed [{}], affected [{}]",
                    changed.join(", "),
                    affected.join(", ")
                ),
                IncrementalPackageSelection::Full { reason } => println!(
                    "incremental std test selection fell back to the full whitelist: {reason}"
                ),
            }
            select_std_packages(all_packages, &selection)
        }
    };

    println!(
        "running std tests for {} package(s) from {}",
        packages.len(),
        csv_path.display()
    );
    if packages.is_empty() {
        println!("no affected std test packages selected");
        return Ok(());
    }

    let mut runner = ProcessCargoRunner;
    let failed = run_std_tests(&mut runner, &workspace_root, &packages)?;

    if failed.is_empty() {
        println!("all std tests passed");
        return Ok(());
    }

    eprintln!(
        "std tests failed for {} package(s): {}",
        failed.len(),
        failed.join(", ")
    );
    bail!("std test run failed")
}

fn workspace_packages(metadata: &Metadata) -> Vec<Package> {
    metadata
        .packages
        .iter()
        .filter(|package| metadata.workspace_members.contains(&package.id))
        .cloned()
        .collect()
}

fn select_std_packages(
    mut packages: Vec<String>,
    selection: &IncrementalPackageSelection,
) -> Vec<String> {
    let IncrementalPackageSelection::Packages { affected, .. } = selection else {
        return packages;
    };
    let affected = affected.iter().map(String::as_str).collect::<HashSet<_>>();
    packages.retain(|package| affected.contains(package.as_str()));
    packages
}

fn workspace_package_names(metadata: &Metadata) -> HashSet<String> {
    metadata
        .packages
        .iter()
        .filter(|pkg| metadata.workspace_members.contains(&pkg.id))
        .map(|pkg| pkg.name.to_string())
        .collect()
}

fn load_std_crates(
    csv_path: &Path,
    known_packages: &HashSet<String>,
) -> anyhow::Result<Vec<String>> {
    let contents = fs::read_to_string(csv_path)
        .with_context(|| format!("failed to read {}", csv_path.display()))?;
    parse_std_crates_csv(&contents, known_packages)
}

fn parse_std_crates_csv(
    contents: &str,
    known_packages: &HashSet<String>,
) -> anyhow::Result<Vec<String>> {
    let mut lines = contents.lines().enumerate().filter_map(|(idx, raw)| {
        let line = raw.trim();
        (!line.is_empty()).then_some((idx + 1, line))
    });

    let Some((header_line, header)) = lines.next() else {
        bail!("std crate csv is empty")
    };
    let header = header.trim_start_matches('\u{feff}');
    if header != "package" {
        bail!(
            "invalid header at line {}: expected `package`, found `{}`",
            header_line,
            header
        );
    }

    let mut packages = Vec::new();
    let mut seen = HashSet::new();
    for (line_no, package) in lines {
        if !known_packages.contains(package) {
            bail!(
                "unknown workspace package `{}` at line {}",
                package,
                line_no
            );
        }
        if !seen.insert(package.to_owned()) {
            bail!("duplicate package `{}` at line {}", package, line_no);
        }
        packages.push(package.to_owned());
    }

    Ok(packages)
}

fn run_std_tests<R: CargoRunner>(
    runner: &mut R,
    workspace_root: &Path,
    packages: &[String],
) -> anyhow::Result<Vec<String>> {
    let mut failed = Vec::new();

    for (index, package) in packages.iter().enumerate() {
        let passed = if let Some(profiles) = package_feature_profiles(package) {
            println!(
                "[{}/{}] running {} std test profile(s) for {}",
                index + 1,
                packages.len(),
                profiles.len(),
                package
            );
            run_feature_profiles(runner, workspace_root, package, profiles)?
        } else {
            let invocation = CargoTestInvocation::default_for(package);
            println!(
                "[{}/{}] cargo {}",
                index + 1,
                packages.len(),
                invocation.args().join(" ")
            );
            runner.run(workspace_root, &invocation)?.success
        };

        if passed {
            println!("ok: {}", package);
        } else {
            eprintln!("failed: {}", package);
            failed.push(package.clone());
        }
    }

    Ok(failed)
}

fn package_feature_profiles(package: &str) -> Option<&'static [PackageFeatureProfile]> {
    match package {
        "arm_vgic"
        | "axdevice"
        | "axfs-ng-vfs"
        | "rsext4"
        | "scope-local"
        | "ax-sync"
        | "axvm"
        | "ax-display"
        | "ax-input"
        | "ax-ipi"
        | "ax-log"
        | "ax-runtime"
        | "ax-api"
        | "rdrive"
        | "axpoll"
        | "ax-net"
        | "dma-api"
        | "buddy-slab-allocator" => Some(HOST_TEST_FEATURE_PROFILES),
        "ax-io" | "axbacktrace" => Some(ALLOC_FEATURE_PROFILES),
        "ax-task" => Some(AX_TASK_FEATURE_PROFILES),
        "ax-driver" => Some(AX_DRIVER_FEATURE_PROFILES),
        "axvisor" => Some(FS_FEATURE_PROFILES),
        "starry-kernel" => Some(STARRY_KERNEL_FEATURE_PROFILES),
        _ => None,
    }
}

fn run_feature_profiles<R: CargoRunner>(
    runner: &mut R,
    workspace_root: &Path,
    package: &str,
    profiles: &[PackageFeatureProfile],
) -> anyhow::Result<bool> {
    let mut passed = true;

    for profile in profiles {
        if !run_feature_profile(runner, workspace_root, package, profile)? {
            passed = false;
        }
    }

    Ok(passed)
}

fn run_feature_profile<R: CargoRunner>(
    runner: &mut R,
    workspace_root: &Path,
    package: &str,
    profile: &PackageFeatureProfile,
) -> anyhow::Result<bool> {
    if !profile.expected_tests.is_empty() {
        let list_invocation =
            CargoTestInvocation::for_profile(package, profile, CargoTestAction::List);
        println!("cargo {}", list_invocation.args().join(" "));
        let listed = runner.run(workspace_root, &list_invocation)?;
        if !listed.success {
            eprintln!(
                "profile `{}` failed while listing filtered tests",
                profile.name
            );
            return Ok(false);
        }
        if let Err(err) = validate_discovered_tests(profile, &listed.stdout) {
            eprintln!("profile `{}` test discovery failed: {err:#}", profile.name);
            return Ok(false);
        }
    }

    let run_invocation = CargoTestInvocation::for_profile(package, profile, CargoTestAction::Run);
    println!("cargo {}", run_invocation.args().join(" "));
    let executed = runner.run(workspace_root, &run_invocation)?;
    if !executed.success {
        eprintln!("profile `{}` filtered tests failed", profile.name);
    }
    Ok(executed.success)
}

fn validate_discovered_tests(
    profile: &PackageFeatureProfile,
    listed_stdout: &str,
) -> anyhow::Result<()> {
    let discovered = parse_listed_tests(listed_stdout);
    let expected = profile
        .expected_tests
        .iter()
        .map(|test| (*test).to_owned())
        .collect::<BTreeSet<_>>();

    if discovered.is_empty() {
        bail!(
            "expected [{}], but the filtered command discovered 0 tests",
            expected.iter().cloned().collect::<Vec<_>>().join(", ")
        );
    }
    if discovered != expected {
        bail!(
            "expected [{}], discovered [{}]",
            expected.iter().cloned().collect::<Vec<_>>().join(", "),
            discovered.iter().cloned().collect::<Vec<_>>().join(", ")
        );
    }

    Ok(())
}

fn parse_listed_tests(listed_stdout: &str) -> BTreeSet<String> {
    listed_stdout
        .lines()
        .filter_map(|line| line.trim().strip_suffix(": test"))
        .map(str::to_owned)
        .collect()
}

trait CargoRunner {
    fn run(
        &mut self,
        workspace_root: &Path,
        invocation: &CargoTestInvocation,
    ) -> anyhow::Result<CargoRunOutput>;
}

struct ProcessCargoRunner;

impl CargoRunner for ProcessCargoRunner {
    fn run(
        &mut self,
        workspace_root: &Path,
        invocation: &CargoTestInvocation,
    ) -> anyhow::Result<CargoRunOutput> {
        let args = invocation.args();
        if invocation.action == CargoTestAction::Run {
            return Ok(CargoRunOutput {
                success: run_cargo_status(workspace_root, &args)?,
                stdout: String::new(),
            });
        }

        let output = Command::new("cargo")
            .current_dir(workspace_root)
            .args(&args)
            .output()
            .with_context(|| format!("failed to spawn `cargo {}`", args.join(" ")))?;
        io::stdout()
            .write_all(&output.stdout)
            .context("failed to print cargo stdout")?;
        io::stderr()
            .write_all(&output.stderr)
            .context("failed to print cargo stderr")?;

        Ok(CargoRunOutput {
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, path::PathBuf};

    use super::*;

    fn known_packages() -> HashSet<String> {
        HashSet::from(["ax-api".to_string(), "ax-hal".to_string()])
    }

    struct FakeCargoRunner {
        results: HashMap<CargoTestInvocation, CargoRunOutput>,
        invocations: Vec<(PathBuf, CargoTestInvocation)>,
    }

    impl FakeCargoRunner {
        fn succeeding() -> Self {
            Self {
                results: HashMap::new(),
                invocations: Vec::new(),
            }
        }

        fn with_status(mut self, invocation: CargoTestInvocation, success: bool) -> Self {
            self.results.insert(
                invocation,
                CargoRunOutput {
                    success,
                    stdout: String::new(),
                },
            );
            self
        }

        fn with_listing(mut self, profile: &PackageFeatureProfile, tests: &[&str]) -> Self {
            self.results.insert(
                CargoTestInvocation::for_profile("ax-task", profile, CargoTestAction::List),
                CargoRunOutput {
                    success: true,
                    stdout: render_test_list(tests),
                },
            );
            self
        }

        fn with_ax_task_discovery(mut self) -> Self {
            for profile in AX_TASK_FEATURE_PROFILES {
                self = self.with_listing(profile, profile.expected_tests);
            }
            self
        }
    }

    impl CargoRunner for FakeCargoRunner {
        fn run(
            &mut self,
            workspace_root: &Path,
            invocation: &CargoTestInvocation,
        ) -> anyhow::Result<CargoRunOutput> {
            self.invocations
                .push((workspace_root.to_path_buf(), invocation.clone()));
            Ok(self
                .results
                .get(invocation)
                .cloned()
                .unwrap_or(CargoRunOutput {
                    success: true,
                    stdout: String::new(),
                }))
        }
    }

    fn render_test_list(tests: &[&str]) -> String {
        let mut output = tests
            .iter()
            .map(|test| format!("{test}: test"))
            .collect::<Vec<_>>()
            .join("\n");
        output.push_str(&format!("\n\n{} tests, 0 benchmarks\n", tests.len()));
        output
    }

    #[test]
    fn parses_valid_std_csv() {
        let packages =
            parse_std_crates_csv("package\nax-api\nax-hal\n", &known_packages()).unwrap();

        assert_eq!(packages, vec!["ax-api".to_string(), "ax-hal".to_string()]);
    }

    #[test]
    fn incremental_selection_keeps_affected_whitelist_order() {
        let packages = ["ax-api", "ax-hal", "ax-task"].map(str::to_string).to_vec();
        let selection = IncrementalPackageSelection::Packages {
            changed: vec!["ax-task".to_string()],
            affected: vec!["ax-task".to_string(), "ax-api".to_string()],
        };

        let selected = select_std_packages(packages, &selection);

        assert_eq!(selected, vec!["ax-api".to_string(), "ax-task".to_string()]);
    }

    #[test]
    fn incremental_selection_accepts_no_affected_std_packages() {
        let packages = vec!["ax-api".to_string(), "ax-hal".to_string()];
        let selection = IncrementalPackageSelection::Packages {
            changed: vec!["standalone".to_string()],
            affected: vec!["standalone".to_string()],
        };

        let selected = select_std_packages(packages, &selection);

        assert!(selected.is_empty());
    }

    #[test]
    fn incremental_full_fallback_keeps_every_std_package() {
        let packages = vec!["ax-api".to_string(), "ax-hal".to_string()];
        let selection = IncrementalPackageSelection::Full {
            reason: "fixture".to_string(),
        };

        let selected = select_std_packages(packages.clone(), &selection);

        assert_eq!(selected, packages);
    }

    #[test]
    fn parses_std_csv_with_blank_lines() {
        let packages =
            parse_std_crates_csv("\npackage\n\nax-api\n\nax-hal\n", &known_packages()).unwrap();

        assert_eq!(packages, vec!["ax-api".to_string(), "ax-hal".to_string()]);
    }

    #[test]
    fn rejects_empty_std_csv() {
        let err = parse_std_crates_csv("", &known_packages()).unwrap_err();

        assert!(err.to_string().contains("std crate csv is empty"));
    }

    #[test]
    fn rejects_invalid_header() {
        let err = parse_std_crates_csv("crate\nax-api\n", &known_packages()).unwrap_err();

        assert!(err.to_string().contains("invalid header"));
    }

    #[test]
    fn rejects_unknown_package() {
        let err = parse_std_crates_csv("package\nunknown\n", &known_packages()).unwrap_err();

        assert!(
            err.to_string()
                .contains("unknown workspace package `unknown`")
        );
    }

    #[test]
    fn rejects_duplicate_package() {
        let err = parse_std_crates_csv("package\nax-api\nax-api\n", &known_packages()).unwrap_err();

        assert!(err.to_string().contains("duplicate package `ax-api`"));
    }

    #[test]
    fn workspace_package_name_extraction_reads_current_workspace() {
        let metadata = cargo_metadata::MetadataCommand::new()
            .no_deps()
            .exec()
            .unwrap();
        let names = workspace_package_names(&metadata);

        assert!(names.contains("axbuild"));
        assert!(names.contains("tg-xtask"));
    }

    #[test]
    fn std_test_runner_collects_all_failures() {
        let root = PathBuf::from("/tmp/workspace");
        let packages = vec!["ax-api".to_string(), "ax-hal".to_string()];
        let mut runner = FakeCargoRunner::succeeding()
            .with_status(CargoTestInvocation::default_for("ax-hal"), false);

        let failed = run_std_tests(&mut runner, &root, &packages).unwrap();

        assert_eq!(failed, vec!["ax-hal".to_string()]);
        assert_eq!(
            runner.invocations,
            vec![
                (
                    root.clone(),
                    CargoTestInvocation::for_profile(
                        "ax-api",
                        &HOST_TEST_FEATURE_PROFILES[0],
                        CargoTestAction::Run,
                    ),
                ),
                (root, CargoTestInvocation::default_for("ax-hal")),
            ]
        );
    }

    #[test]
    fn std_test_runner_returns_empty_failures_when_all_pass() {
        let root = PathBuf::from("/tmp/workspace");
        let packages = vec!["ax-api".to_string(), "ax-hal".to_string()];
        let mut runner = FakeCargoRunner::succeeding();

        let failed = run_std_tests(&mut runner, &root, &packages).unwrap();

        assert!(failed.is_empty());
    }

    #[test]
    fn ordinary_package_keeps_default_cargo_test_command() {
        let root = PathBuf::from("/tmp/workspace");
        let packages = vec!["ax-hal".to_string()];
        let mut runner = FakeCargoRunner::succeeding();

        let failed = run_std_tests(&mut runner, &root, &packages).unwrap();

        assert!(failed.is_empty());
        assert_eq!(runner.invocations.len(), 1);
        assert_eq!(runner.invocations[0].1.args(), vec!["test", "-p", "ax-hal"]);
    }

    #[test]
    fn ax_driver_uses_visionfive2_mmc_feature_profile() {
        let root = PathBuf::from("/tmp/workspace");
        let packages = vec!["ax-driver".to_string()];
        let mut runner = FakeCargoRunner::succeeding();

        let failed = run_std_tests(&mut runner, &root, &packages).unwrap();

        assert!(failed.is_empty());
        assert_eq!(
            runner.invocations[0].1.args(),
            vec![
                "test",
                "-p",
                "ax-driver",
                "--features",
                "host-test,rtc,starfive-jh7110-dwmmc"
            ]
        );
    }

    #[test]
    fn ax_task_uses_pure_and_task_initialization_feature_profiles() {
        let root = PathBuf::from("/tmp/workspace");
        let packages = vec!["ax-task".to_string()];
        let mut runner = FakeCargoRunner::succeeding().with_ax_task_discovery();

        let failed = run_std_tests(&mut runner, &root, &packages).unwrap();

        assert!(failed.is_empty());
        let args = runner
            .invocations
            .iter()
            .map(|(_, invocation)| invocation.args())
            .collect::<Vec<_>>();
        assert_eq!(
            args,
            vec![
                vec![
                    "test",
                    "-p",
                    "ax-task",
                    "--features",
                    "host-test,multitask,irq",
                    "std_tests::",
                    "--",
                    "--list",
                ],
                vec![
                    "test",
                    "-p",
                    "ax-task",
                    "--features",
                    "host-test,multitask,irq",
                    "std_tests::",
                ],
                vec![
                    "test",
                    "-p",
                    "ax-task",
                    "--features",
                    "host-test,multitask",
                    "task_initialization_precedes_scheduling",
                    "--",
                    "--list",
                ],
                vec![
                    "test",
                    "-p",
                    "ax-task",
                    "--features",
                    "host-test,multitask",
                    "task_initialization_precedes_scheduling",
                ],
            ]
        );
        assert!(!args.contains(&vec!["test".into(), "-p".into(), "ax-task".into()]));
    }

    #[test]
    fn ax_sync_host_packages_use_host_test_feature_profile() {
        let root = PathBuf::from("/tmp/workspace");
        let packages = [
            "arm_vgic",
            "axdevice",
            "axfs-ng-vfs",
            "rsext4",
            "scope-local",
            "ax-sync",
            "buddy-slab-allocator",
        ]
        .map(str::to_string)
        .to_vec();
        let mut runner = FakeCargoRunner::succeeding();

        let failed = run_std_tests(&mut runner, &root, &packages).unwrap();

        assert!(failed.is_empty());
        let args = runner
            .invocations
            .iter()
            .map(|(_, invocation)| invocation.args())
            .collect::<Vec<_>>();
        assert_eq!(
            args,
            vec![
                vec!["test", "-p", "arm_vgic", "--features", "host-test"],
                vec!["test", "-p", "axdevice", "--features", "host-test"],
                vec!["test", "-p", "axfs-ng-vfs", "--features", "host-test"],
                vec!["test", "-p", "rsext4", "--features", "host-test"],
                vec!["test", "-p", "scope-local", "--features", "host-test"],
                vec!["test", "-p", "ax-sync", "--features", "host-test"],
                vec![
                    "test",
                    "-p",
                    "buddy-slab-allocator",
                    "--features",
                    "host-test",
                ],
            ]
        );
    }

    #[test]
    fn runtime_aggregate_packages_run_only_their_standard_test_subset() {
        let root = PathBuf::from("/tmp/workspace");
        let packages = ["axvisor", "starry-kernel"].map(str::to_string).to_vec();
        let mut runner = FakeCargoRunner::succeeding();

        let failed = run_std_tests(&mut runner, &root, &packages).unwrap();

        assert!(failed.is_empty());
        let args = runner
            .invocations
            .iter()
            .map(|(_, invocation)| invocation.args())
            .collect::<Vec<_>>();
        assert_eq!(
            args,
            vec![
                vec!["test", "-p", "axvisor", "--features", "fs"],
                vec!["test", "-p", "starry-kernel", "std_tests::"],
            ]
        );
    }

    #[test]
    fn transitive_platform_consumers_use_host_test_feature_profile() {
        let root = PathBuf::from("/tmp/workspace");
        let packages = [
            "axvm",
            "ax-display",
            "ax-input",
            "ax-ipi",
            "ax-log",
            "ax-runtime",
            "ax-api",
        ]
        .map(str::to_string)
        .to_vec();
        let mut runner = FakeCargoRunner::succeeding();

        let failed = run_std_tests(&mut runner, &root, &packages).unwrap();

        assert!(failed.is_empty());
        let args = runner
            .invocations
            .iter()
            .map(|(_, invocation)| invocation.args())
            .collect::<Vec<_>>();
        assert_eq!(
            args,
            packages
                .iter()
                .map(|package| {
                    vec![
                        "test".to_string(),
                        "-p".to_string(),
                        package.clone(),
                        "--features".to_string(),
                        "host-test".to_string(),
                    ]
                })
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn profile_discovery_mismatch_fails_without_running_that_profile() {
        let root = PathBuf::from("/tmp/workspace");
        let packages = vec!["ax-task".to_string()];
        let pure_profile = &AX_TASK_FEATURE_PROFILES[0];
        let initialization_profile = &AX_TASK_FEATURE_PROFILES[1];
        let mut runner = FakeCargoRunner::succeeding()
            .with_ax_task_discovery()
            .with_listing(pure_profile, &["api::std_tests::unexpected"])
            .with_listing(
                initialization_profile,
                initialization_profile.expected_tests,
            );

        let failed = run_std_tests(&mut runner, &root, &packages).unwrap();

        assert_eq!(failed, vec!["ax-task"]);
        assert!(!runner.invocations.iter().any(|(_, invocation)| {
            invocation
                == &CargoTestInvocation::for_profile("ax-task", pure_profile, CargoTestAction::Run)
        }));
        assert!(runner.invocations.iter().any(|(_, invocation)| {
            invocation
                == &CargoTestInvocation::for_profile(
                    "ax-task",
                    initialization_profile,
                    CargoTestAction::Run,
                )
        }));
    }

    #[test]
    fn profile_discovery_rejects_zero_tests() {
        let err = validate_discovered_tests(&AX_TASK_FEATURE_PROFILES[0], "0 tests, 0 benchmarks")
            .unwrap_err();

        assert!(err.to_string().contains("discovered 0 tests"));
    }

    #[test]
    fn cargo_execution_failures_are_aggregated_across_profiles_and_packages() {
        let root = PathBuf::from("/tmp/workspace");
        let packages = vec!["ax-task".to_string(), "ax-api".to_string()];
        let failed_profile = &AX_TASK_FEATURE_PROFILES[0];
        let mut runner = FakeCargoRunner::succeeding()
            .with_ax_task_discovery()
            .with_status(
                CargoTestInvocation::for_profile("ax-task", failed_profile, CargoTestAction::Run),
                false,
            )
            .with_status(
                CargoTestInvocation::for_profile(
                    "ax-api",
                    &HOST_TEST_FEATURE_PROFILES[0],
                    CargoTestAction::Run,
                ),
                false,
            );

        let failed = run_std_tests(&mut runner, &root, &packages).unwrap();

        assert_eq!(failed, vec!["ax-task", "ax-api"]);
        assert_eq!(runner.invocations.len(), 5);
    }
}
