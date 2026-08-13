use std::{collections::HashSet, fs, path::Path};

use anyhow::{Context, bail};
use cargo_metadata::Metadata;

use crate::support::process::run_cargo_status;

const STD_CRATES_CSV: &str = "scripts/test/std_crates.csv";
#[derive(Clone, Copy, Debug)]
struct PackageFeatureProfile {
    name: &'static str,
    features: &'static [&'static str],
}

const HOST_TEST_FEATURE_PROFILES: &[PackageFeatureProfile] = &[PackageFeatureProfile {
    name: "host-test",
    features: &["host-test"],
}];

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct CargoTestInvocation {
    package: String,
    features: Vec<String>,
}

impl CargoTestInvocation {
    fn default_for(package: &str) -> Self {
        Self {
            package: package.to_owned(),
            features: Vec::new(),
        }
    }

    fn for_profile(package: &str, profile: &PackageFeatureProfile) -> Self {
        Self {
            package: package.to_owned(),
            features: profile
                .features
                .iter()
                .map(|feature| (*feature).to_owned())
                .collect(),
        }
    }

    fn args(&self) -> Vec<String> {
        let mut args = vec!["test".into(), "-p".into(), self.package.clone()];
        if !self.features.is_empty() {
            args.push("--features".into());
            args.push(self.features.join(","));
        }
        args
    }
}

#[derive(Clone, Debug)]
struct CargoRunOutput {
    success: bool,
}

pub(crate) fn run_std_test_command() -> anyhow::Result<()> {
    let workspace_manifest = crate::context::workspace_manifest_path()?;
    let metadata = crate::context::workspace_metadata_root_manifest(&workspace_manifest)
        .context("failed to load cargo metadata")?;
    let workspace_root = metadata.workspace_root.clone().into_std_path_buf();
    let known_packages = workspace_package_names(&metadata);
    let csv_path = workspace_root.join(STD_CRATES_CSV);
    let packages = load_std_crates(&csv_path, &known_packages)?;

    println!(
        "running std tests for {} package(s) from {}",
        packages.len(),
        csv_path.display()
    );

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
        "ax-sync" | "ax-display" | "ax-input" | "ax-ipi" | "ax-log" | "ax-api" | "rdrive" => {
            Some(HOST_TEST_FEATURE_PROFILES)
        }
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
    let run_invocation = CargoTestInvocation::for_profile(package, profile);
    println!("cargo {}", run_invocation.args().join(" "));
    let executed = runner.run(workspace_root, &run_invocation)?;
    if !executed.success {
        eprintln!("profile `{}` tests failed", profile.name);
    }
    Ok(executed.success)
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
        Ok(CargoRunOutput {
            success: run_cargo_status(workspace_root, &args)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, path::PathBuf};

    use super::*;

    fn known_packages() -> HashSet<String> {
        HashSet::from([
            "ax-api".to_string(),
            "ax-hal".to_string(),
            "starry-process".to_string(),
        ])
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
            self.results.insert(invocation, CargoRunOutput { success });
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
                .unwrap_or(CargoRunOutput { success: true }))
        }
    }

    #[test]
    fn parses_valid_std_csv() {
        let packages =
            parse_std_crates_csv("package\nax-api\nax-hal\n", &known_packages()).unwrap();

        assert_eq!(packages, vec!["ax-api".to_string(), "ax-hal".to_string()]);
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
        let packages = vec![
            "ax-api".to_string(),
            "ax-hal".to_string(),
            "starry-process".to_string(),
        ];
        let mut runner = FakeCargoRunner::succeeding()
            .with_status(CargoTestInvocation::default_for("ax-hal"), false)
            .with_status(CargoTestInvocation::default_for("starry-process"), false);

        let failed = run_std_tests(&mut runner, &root, &packages).unwrap();

        assert_eq!(
            failed,
            vec!["ax-hal".to_string(), "starry-process".to_string()]
        );
        assert_eq!(
            runner.invocations,
            vec![
                (
                    root.clone(),
                    CargoTestInvocation::for_profile("ax-api", &HOST_TEST_FEATURE_PROFILES[0]),
                ),
                (root.clone(), CargoTestInvocation::default_for("ax-hal")),
                (root, CargoTestInvocation::default_for("starry-process")),
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
        let packages = vec!["starry-process".to_string()];
        let mut runner = FakeCargoRunner::succeeding();

        let failed = run_std_tests(&mut runner, &root, &packages).unwrap();

        assert!(failed.is_empty());
        assert_eq!(runner.invocations.len(), 1);
        assert_eq!(
            runner.invocations[0].1.args(),
            vec!["test", "-p", "starry-process"]
        );
    }

    #[test]
    fn ax_sync_host_packages_use_host_test_feature_profile() {
        let root = PathBuf::from("/tmp/workspace");
        let packages = ["ax-sync"].map(str::to_string).to_vec();
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
            vec![vec!["test", "-p", "ax-sync", "--features", "host-test"]]
        );
    }

    #[test]
    fn transitive_platform_consumers_use_host_test_feature_profile() {
        let root = PathBuf::from("/tmp/workspace");
        let packages = [
            "ax-display",
            "ax-input",
            "ax-ipi",
            "ax-log",
            "ax-api",
            "rdrive",
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
    fn cargo_execution_failures_are_aggregated_across_profiles_and_packages() {
        let root = PathBuf::from("/tmp/workspace");
        let packages = vec!["ax-sync".to_string(), "ax-api".to_string()];
        let mut runner = FakeCargoRunner::succeeding()
            .with_status(
                CargoTestInvocation::for_profile("ax-sync", &HOST_TEST_FEATURE_PROFILES[0]),
                false,
            )
            .with_status(
                CargoTestInvocation::for_profile("ax-api", &HOST_TEST_FEATURE_PROFILES[0]),
                false,
            );

        let failed = run_std_tests(&mut runner, &root, &packages).unwrap();

        assert_eq!(failed, vec!["ax-sync", "ax-api"]);
        assert_eq!(runner.invocations.len(), 2);
    }
}
