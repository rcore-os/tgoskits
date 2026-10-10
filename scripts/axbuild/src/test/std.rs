use std::{collections::HashSet, fs, path::Path};

use anyhow::{Context, bail};
use cargo_metadata::{Metadata, Package};
use clap::Args;

use crate::support::{git::IncrementalPackageSelection, process::run_cargo_output};

const STD_CRATES_CSV: &str = "scripts/test/std_crates.csv";
#[derive(Args, Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct StdTestArgs {
    /// Run std tests only for workspace packages affected since the git ref
    #[arg(long, value_name = "REF")]
    pub(crate) since: Option<String>,
}

#[derive(Clone, Copy, Debug)]
struct PackageFeatureProfile {
    name: &'static str,
    no_default_features: bool,
    features: &'static [&'static str],
}

const AX_HAL_FEATURE_PROFILES: &[PackageFeatureProfile] = &[PackageFeatureProfile {
    name: "host-test",
    no_default_features: false,
    features: &["host-test"],
}];

const AX_DRIVER_FEATURE_PROFILES: &[PackageFeatureProfile] = &[
    PackageFeatureProfile {
        name: "host-test+rtc+starfive-jh7110-dwmmc",
        no_default_features: false,
        features: &["host-test", "rtc", "starfive-jh7110-dwmmc"],
    },
    PackageFeatureProfile {
        name: "pci-fdt-irq-capability",
        no_default_features: false,
        features: &["pci"],
    },
    PackageFeatureProfile {
        name: "host-test+rk3588-cpufreq",
        no_default_features: false,
        features: &["host-test", "rk3588-cpufreq"],
    },
];

// Exercise the rdif implementation under its real feature profile.
const VIRTIO_GPU_FEATURE_PROFILES: &[PackageFeatureProfile] = &[PackageFeatureProfile {
    name: "rdif",
    no_default_features: false,
    features: &["rdif"],
}];

const ACPICA_FEATURE_PROFILES: &[PackageFeatureProfile] = &[PackageFeatureProfile {
    name: "host-test",
    no_default_features: false,
    features: &["host-test"],
}];

const HOST_TEST_FEATURE_PROFILES: &[PackageFeatureProfile] = &[PackageFeatureProfile {
    name: "host-test",
    no_default_features: false,
    features: &["host-test"],
}];

const AXVM_FEATURE_PROFILES: &[PackageFeatureProfile] = &[HOST_TEST_FEATURE_PROFILES[0]];

const ALLOC_FEATURE_PROFILES: &[PackageFeatureProfile] = &[PackageFeatureProfile {
    name: "alloc",
    no_default_features: false,
    features: &["alloc"],
}];

const AX_FS_NG_FEATURE_PROFILES: &[PackageFeatureProfile] = &[
    PackageFeatureProfile {
        name: "host-test+vfs+fat+ext4",
        no_default_features: false,
        features: &["host-test", "vfs", "fat", "ext4"],
    },
    PackageFeatureProfile {
        name: "host-test",
        no_default_features: false,
        features: &["host-test"],
    },
];

const NVME_FEATURE_PROFILES: &[PackageFeatureProfile] = &[PackageFeatureProfile {
    name: "default",
    no_default_features: false,
    features: &[],
}];

const SDMMC_RDIF_FEATURE_PROFILES: &[PackageFeatureProfile] = &[PackageFeatureProfile {
    name: "rdif",
    no_default_features: true,
    features: &["rdif"],
}];

const AIC8800_FEATURE_PROFILES: &[PackageFeatureProfile] = &[PackageFeatureProfile {
    name: "host-test+rdif",
    no_default_features: false,
    features: &["host-test", "rdif"],
}];

const AXBUILD_FEATURE_PROFILES: &[PackageFeatureProfile] = &[PackageFeatureProfile {
    name: "default",
    no_default_features: false,
    features: &[],
}];
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct CargoTestInvocation {
    package: String,
    no_default_features: bool,
    features: Vec<String>,
}

impl CargoTestInvocation {
    fn default_for(package: &str) -> Self {
        Self {
            package: package.to_owned(),
            no_default_features: false,
            features: Vec::new(),
        }
    }

    fn for_profile(package: &str, profile: &PackageFeatureProfile) -> Self {
        Self {
            package: package.to_owned(),
            no_default_features: profile.no_default_features,
            features: profile
                .features
                .iter()
                .map(|feature| (*feature).to_owned())
                .collect(),
        }
    }

    fn args(&self) -> Vec<String> {
        let mut args = vec!["test".into(), "-p".into(), self.package.clone()];
        if self.no_default_features {
            args.push("--no-default-features".into());
        }
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
    tests_run: usize,
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
            let output = runner.run(workspace_root, &invocation)?;
            output.success && output.tests_run > 0
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
        | "x86_vlapic"
        | "axdevice"
        | "axfs-ng-vfs"
        | "rsext4"
        | "scope-local"
        | "ax-sync"
        | "ax-display"
        | "ax-input"
        | "ax-ipi"
        | "ax-log"
        | "ax-runtime"
        | "ax-api"
        | "rdrive"
        | "ax-net"
        | "dma-api"
        | "buddy-slab-allocator" => Some(HOST_TEST_FEATURE_PROFILES),
        "axvm" => Some(AXVM_FEATURE_PROFILES),
        "ax-fs-ng" => Some(AX_FS_NG_FEATURE_PROFILES),
        "ax-io" | "axbacktrace" => Some(ALLOC_FEATURE_PROFILES),
        "ax-hal" => Some(AX_HAL_FEATURE_PROFILES),
        "ax-driver" => Some(AX_DRIVER_FEATURE_PROFILES),
        "nvme-driver" => Some(NVME_FEATURE_PROFILES),
        "acpica-interpreter" => Some(ACPICA_FEATURE_PROFILES),
        "sdmmc-protocol" => Some(SDMMC_RDIF_FEATURE_PROFILES),
        "aic8800" => Some(AIC8800_FEATURE_PROFILES),
        "axbuild" => Some(AXBUILD_FEATURE_PROFILES),
        "virtio-gpu" => Some(VIRTIO_GPU_FEATURE_PROFILES),
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
    let invocation = CargoTestInvocation::for_profile(package, profile);
    println!("cargo {}", invocation.args().join(" "));
    let executed = runner.run(workspace_root, &invocation)?;
    if !executed.success || executed.tests_run == 0 {
        eprintln!("profile `{}` tests failed", profile.name);
    }
    Ok(executed.success && executed.tests_run > 0)
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
        let output = run_cargo_output(workspace_root, &args)?;
        print!("{}", String::from_utf8_lossy(&output.stdout));
        eprint!("{}", String::from_utf8_lossy(&output.stderr));
        let tests_run = count_test_cases(&output.stdout);
        Ok(CargoRunOutput {
            success: output.status.success() && tests_run > 0,
            tests_run,
        })
    }
}

fn count_test_cases(output: &[u8]) -> usize {
    String::from_utf8_lossy(output)
        .lines()
        .filter_map(|line| {
            let rest = line.trim_start().strip_prefix("running ")?;
            let (count, suffix) = rest.split_once(' ')?;
            if suffix == "test" || suffix == "tests" {
                count.parse::<usize>().ok()
            } else {
                None
            }
        })
        .sum()
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
                    tests_run: 1,
                },
            );
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
                    tests_run: 1,
                }))
        }
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
    fn rejects_unknown_package() {
        let err = parse_std_crates_csv("package\nunknown\n", &known_packages()).unwrap_err();

        assert!(
            err.to_string()
                .contains("unknown workspace package `unknown`")
        );
    }

    #[test]
    fn std_test_runner_collects_all_failures() {
        let root = PathBuf::from("/tmp/workspace");
        let packages = vec!["alpha".to_string(), "beta".to_string(), "gamma".to_string()];
        let mut runner = FakeCargoRunner::succeeding()
            .with_status(CargoTestInvocation::default_for("alpha"), false)
            .with_status(CargoTestInvocation::default_for("gamma"), false);

        let failed = run_std_tests(&mut runner, &root, &packages).unwrap();

        assert_eq!(failed, vec!["alpha", "gamma"]);
        assert_eq!(runner.invocations.len(), packages.len());
    }

    #[test]
    fn profile_invocation_contains_only_real_feature_selection() {
        let profile = PackageFeatureProfile {
            name: "rdif",
            no_default_features: true,
            features: &["rdif"],
        };

        assert_eq!(
            CargoTestInvocation::for_profile("alpha", &profile).args(),
            [
                "test",
                "-p",
                "alpha",
                "--no-default-features",
                "--features",
                "rdif"
            ]
        );
    }

    #[test]
    fn cargo_output_counts_running_test_cases() {
        let output = b"running 3 tests\nrunning 1 test\nrunning 0 tests\n";

        assert_eq!(count_test_cases(output), 4);
    }

    #[test]
    fn zero_test_profile_is_rejected() {
        let root = PathBuf::from("/tmp/workspace");
        let invocation = CargoTestInvocation::default_for("alpha");
        let mut runner = FakeCargoRunner {
            results: HashMap::from([(
                invocation.clone(),
                CargoRunOutput {
                    success: true,
                    tests_run: 0,
                },
            )]),
            invocations: Vec::new(),
        };

        let failed = run_std_tests(&mut runner, &root, &["alpha".to_owned()]).unwrap();

        assert_eq!(failed, vec!["alpha"]);
        assert_eq!(runner.invocations, vec![(root, invocation)]);
    }

    #[test]
    fn cargo_execution_failures_do_not_stop_later_profiles() {
        let root = PathBuf::from("/tmp/workspace");
        const PROFILES: &[PackageFeatureProfile] = &[
            PackageFeatureProfile {
                name: "first",
                no_default_features: false,
                features: &["example-feature"],
            },
            PackageFeatureProfile {
                name: "second",
                no_default_features: true,
                features: &[],
            },
        ];
        let failed_invocation = CargoTestInvocation::for_profile("alpha", &PROFILES[0]);
        let later_invocation = CargoTestInvocation::for_profile("alpha", &PROFILES[1]);
        let mut runner = FakeCargoRunner::succeeding().with_status(failed_invocation, false);

        assert!(!run_feature_profiles(&mut runner, &root, "alpha", PROFILES).unwrap());
        assert_eq!(runner.invocations.last().unwrap().1, later_invocation);
    }
}
