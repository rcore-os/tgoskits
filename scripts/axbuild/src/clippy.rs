use std::{
    collections::{BTreeSet, HashMap, HashSet},
    fs,
    path::Path,
    process::Command,
};

use anyhow::Context;
use cargo_metadata::{Metadata, Package};
use serde_json::Value;

const CLIPPY_CRATES_CSV: &str = "scripts/test/clippy_crates.csv";

pub(crate) fn run_workspace_clippy_command(args: &crate::ClippyArgs) -> anyhow::Result<()> {
    let workspace_manifest = crate::context::workspace_manifest_path()?;
    let metadata = crate::context::workspace_metadata_root_manifest(&workspace_manifest)
        .context("failed to load cargo metadata")?;
    let workspace_root = metadata.workspace_root.clone().into_std_path_buf();
    let all_packages = workspace_packages(&metadata);
    let packages = resolve_requested_packages(args, &workspace_root, &all_packages)?;
    let checks = expand_clippy_checks(&packages);

    println!(
        "running clippy for {} package(s) with {} check(s) from {}",
        packages.len(),
        checks.len(),
        workspace_root.display()
    );

    let mut runner = ProcessCargoRunner;
    let report = run_clippy_checks(&mut runner, &workspace_root, &checks)?;
    print_report_summary(&report, args.all);

    if report.failed_packages().is_empty() {
        println!("all clippy checks passed");
        return Ok(());
    }

    bail!(
        "clippy failed for {} package(s): {}",
        report.failed_packages().len(),
        report.failed_packages().join(", ")
    )
}

fn workspace_packages(metadata: &Metadata) -> Vec<Package> {
    let workspace_members: HashSet<_> = metadata.workspace_members.iter().cloned().collect();
    let mut packages: Vec<_> = metadata
        .packages
        .iter()
        .filter(|pkg| workspace_members.contains(&pkg.id))
        .cloned()
        .collect();
    packages.sort_by(|left, right| left.name.cmp(&right.name));
    packages
}

fn resolve_requested_packages(
    args: &crate::ClippyArgs,
    workspace_root: &Path,
    all_packages: &[Package],
) -> anyhow::Result<Vec<Package>> {
    let package_lookup: HashMap<_, _> = all_packages
        .iter()
        .map(|pkg| (pkg.name.as_str(), pkg.clone()))
        .collect();
    let known_packages: HashSet<_> = all_packages.iter().map(|pkg| pkg.name.as_str()).collect();

    let package_names = if !args.packages.is_empty() {
        validate_requested_packages(&args.packages, &known_packages)?
    } else if args.all {
        all_packages
            .iter()
            .map(|pkg| pkg.name.to_string())
            .collect()
    } else {
        let csv_path = workspace_root.join(CLIPPY_CRATES_CSV);
        load_clippy_crates(&csv_path, &known_packages)?
    };

    package_names
        .into_iter()
        .map(|package| {
            package_lookup
                .get(package.as_str())
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("workspace package `{package}` not found"))
        })
        .collect()
}

fn validate_requested_packages(
    requested: &[String],
    known_packages: &HashSet<&str>,
) -> anyhow::Result<Vec<String>> {
    let mut unique = HashSet::new();
    let mut packages = Vec::new();

    for package in requested {
        if !known_packages.contains(package.as_str()) {
            bail!("unknown workspace package `{package}` requested via --package");
        }
        if !unique.insert(package.as_str()) {
            bail!("duplicate workspace package `{package}` requested via --package");
        }
        packages.push(package.clone());
    }

    Ok(packages)
}

fn load_clippy_crates(
    csv_path: &Path,
    known_packages: &HashSet<&str>,
) -> anyhow::Result<Vec<String>> {
    let contents = fs::read_to_string(csv_path)
        .with_context(|| format!("failed to read {}", csv_path.display()))?;
    parse_clippy_crates_csv(&contents, known_packages)
}

fn parse_clippy_crates_csv(
    contents: &str,
    known_packages: &HashSet<&str>,
) -> anyhow::Result<Vec<String>> {
    let mut lines = contents.lines().enumerate().filter_map(|(idx, raw)| {
        let line = raw.trim();
        (!line.is_empty()).then_some((idx + 1, line))
    });

    let Some((header_line, header)) = lines.next() else {
        bail!("clippy crate csv is empty")
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

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum ClippyCheckKind {
    Base,
    Feature(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ClippyCheck {
    package: String,
    kind: ClippyCheckKind,
    target: Option<String>,
}

impl ClippyCheck {
    fn cargo_args(&self) -> Vec<String> {
        let mut args = match &self.kind {
            ClippyCheckKind::Base => vec!["clippy".into(), "-p".into(), self.package.clone()],
            ClippyCheckKind::Feature(feature) => vec![
                "clippy".into(),
                "-p".into(),
                self.package.clone(),
                "--no-default-features".into(),
                "--features".into(),
                feature.clone(),
            ],
        };
        if let Some(target) = &self.target {
            args.extend(["--target".into(), target.clone()]);
        }
        args.extend(["--".into(), "-D".into(), "warnings".into()]);
        args
    }

    fn label(&self) -> String {
        let base = match &self.kind {
            ClippyCheckKind::Base => format!("{} (base", self.package),
            ClippyCheckKind::Feature(feature) => {
                format!("{} (feature: {}", self.package, feature)
            }
        };

        match &self.target {
            Some(target) => format!("{base}, target: {target})"),
            None => format!("{base})"),
        }
    }
}

fn docs_rs_targets(package: &Package) -> Vec<String> {
    let Some(docs_rs) = package.metadata.get("docs.rs").and_then(Value::as_object) else {
        return Vec::new();
    };

    let Some(targets) = docs_rs.get("targets").and_then(Value::as_array) else {
        return Vec::new();
    };

    let mut unique_targets = BTreeSet::new();
    for target in targets.iter().filter_map(Value::as_str) {
        unique_targets.insert(target.to_string());
    }

    unique_targets.into_iter().collect()
}

fn expand_clippy_checks(packages: &[Package]) -> Vec<ClippyCheck> {
    let mut checks = Vec::new();

    for package in packages {
        let features: BTreeSet<_> = package
            .features
            .keys()
            .filter(|feature| feature.as_str() != "default")
            .cloned()
            .collect();
        let targets = docs_rs_targets(package);
        let target_iter = if targets.is_empty() {
            vec![None]
        } else {
            targets.into_iter().map(Some).collect()
        };

        for target in target_iter {
            checks.push(ClippyCheck {
                package: package.name.to_string(),
                kind: ClippyCheckKind::Base,
                target: target.clone(),
            });

            for feature in &features {
                checks.push(ClippyCheck {
                    package: package.name.to_string(),
                    kind: ClippyCheckKind::Feature(feature.clone()),
                    target: target.clone(),
                });
            }
        }
    }

    checks
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PackageRunReport {
    package: String,
    total_checks: usize,
    failed_checks: Vec<String>,
}

impl PackageRunReport {
    fn new(package: String) -> Self {
        Self {
            package,
            total_checks: 0,
            failed_checks: Vec::new(),
        }
    }

    fn passed(&self) -> bool {
        self.failed_checks.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ClippyRunReport {
    total_checks: usize,
    passed_checks: usize,
    packages: Vec<PackageRunReport>,
}

impl ClippyRunReport {
    fn passed_packages(&self) -> Vec<String> {
        self.packages
            .iter()
            .filter(|package| package.passed())
            .map(|package| package.package.clone())
            .collect()
    }

    fn failed_packages(&self) -> Vec<String> {
        self.packages
            .iter()
            .filter(|package| !package.passed())
            .map(|package| package.package.clone())
            .collect()
    }

    fn passing_packages_csv(&self) -> String {
        let mut csv = String::from("package\n");
        for package in self.passed_packages() {
            csv.push_str(&package);
            csv.push('\n');
        }
        csv
    }
}

fn run_clippy_checks<R: CargoRunner>(
    runner: &mut R,
    workspace_root: &Path,
    checks: &[ClippyCheck],
) -> anyhow::Result<ClippyRunReport> {
    let mut packages = Vec::new();
    let mut package_indexes = HashMap::new();

    for check in checks {
        if package_indexes.contains_key(check.package.as_str()) {
            continue;
        }
        let index = packages.len();
        packages.push(PackageRunReport::new(check.package.clone()));
        package_indexes.insert(check.package.clone(), index);
    }

    let mut passed_checks = 0;

    for (index, check) in checks.iter().enumerate() {
        let args = check.cargo_args();
        println!("[{}/{}] {}", index + 1, checks.len(), check.label());
        println!("          cargo {}", args.join(" "));

        let success = runner.run_clippy(workspace_root, check)?;
        let package_index = package_indexes[check.package.as_str()];
        let package_report = &mut packages[package_index];
        package_report.total_checks += 1;

        if success {
            passed_checks += 1;
            println!("ok: {}", check.label());
        } else {
            eprintln!("failed: {}", check.label());
            package_report.failed_checks.push(check.label());
        }
    }

    Ok(ClippyRunReport {
        total_checks: checks.len(),
        passed_checks,
        packages,
    })
}

fn print_report_summary(report: &ClippyRunReport, print_csv: bool) {
    println!(
        "clippy summary: {} package(s), {} check(s), {} package(s) passed, {} package(s) failed",
        report.packages.len(),
        report.total_checks,
        report.passed_packages().len(),
        report.failed_packages().len()
    );
    println!(
        "passed checks: {}, failed checks: {}",
        report.passed_checks,
        report.total_checks.saturating_sub(report.passed_checks)
    );

    let failed_packages = report.failed_packages();
    if !failed_packages.is_empty() {
        eprintln!("failed packages: {}", failed_packages.join(", "));
        for package in report.packages.iter().filter(|package| !package.passed()) {
            eprintln!(
                "  {} failed {} check(s): {}",
                package.package,
                package.failed_checks.len(),
                package.failed_checks.join(", ")
            );
        }
    }

    if print_csv {
        println!("passing packages csv:");
        print!("{}", report.passing_packages_csv());
    }
}

trait CargoRunner {
    fn run_clippy(&mut self, workspace_root: &Path, check: &ClippyCheck) -> anyhow::Result<bool>;
}

struct ProcessCargoRunner;

impl CargoRunner for ProcessCargoRunner {
    fn run_clippy(&mut self, workspace_root: &Path, check: &ClippyCheck) -> anyhow::Result<bool> {
        let args = check.cargo_args();
        let status = Command::new("cargo")
            .current_dir(workspace_root)
            .args(&args)
            .status()
            .with_context(|| format!("failed to spawn `cargo {}`", args.join(" ")))?;
        Ok(status.success())
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, path::PathBuf};

    use super::*;

    fn pkg(
        name: &str,
        id: &str,
        features: &[(&str, &[&str])],
        docs_rs_targets: Option<&[&str]>,
    ) -> Package {
        let metadata = docs_rs_targets.map(|targets| {
            serde_json::json!({
                "docs.rs": {
                    "targets": targets,
                }
            })
        });
        let value = serde_json::json!({
            "name": name,
            "version": "0.1.0",
            "id": id,
            "license": null,
            "license_file": null,
            "description": null,
            "source": null,
            "dependencies": [],
            "targets": [{
                "kind": ["lib"],
                "crate_types": ["lib"],
                "name": name,
                "src_path": format!("/tmp/{name}/src/lib.rs"),
                "edition": "2021",
                "doc": true,
                "doctest": true,
                "test": true
            }],
            "features": features.iter().map(|(k, v)| ((*k).to_string(), v.iter().map(|item| (*item).to_string()).collect::<Vec<_>>())).collect::<HashMap<_, _>>(),
            "manifest_path": format!("/tmp/{name}/Cargo.toml"),
            "metadata": metadata,
            "publish": null,
            "authors": [],
            "categories": [],
            "keywords": [],
            "readme": null,
            "repository": null,
            "homepage": null,
            "documentation": null,
            "edition": "2021",
            "links": null,
            "default_run": null,
            "rust_version": null
        });

        serde_json::from_value(value).unwrap()
    }

    fn metadata_with_packages(packages: Vec<Package>, workspace_members: &[&str]) -> Metadata {
        let package_refs = packages;
        let value = serde_json::json!({
            "packages": package_refs,
            "workspace_members": workspace_members,
            "workspace_default_members": workspace_members,
            "resolve": null,
            "target_directory": "/tmp/target",
            "version": 1,
            "workspace_root": "/tmp/ws",
            "metadata": null,
        });

        serde_json::from_value(value).unwrap()
    }

    fn known_packages() -> HashSet<&'static str> {
        HashSet::from(["alpha", "beta", "gamma"])
    }

    fn args(all: bool, packages: &[&str]) -> crate::ClippyArgs {
        crate::ClippyArgs {
            all,
            packages: packages
                .iter()
                .map(|package| (*package).to_string())
                .collect(),
        }
    }

    struct FakeCargoRunner {
        results: HashMap<ClippyCheck, bool>,
        invocations: Vec<(PathBuf, ClippyCheck)>,
    }

    impl FakeCargoRunner {
        fn new(results: &[(ClippyCheck, bool)]) -> Self {
            Self {
                results: results.iter().cloned().collect(),
                invocations: Vec::new(),
            }
        }
    }

    impl CargoRunner for FakeCargoRunner {
        fn run_clippy(
            &mut self,
            workspace_root: &Path,
            check: &ClippyCheck,
        ) -> anyhow::Result<bool> {
            self.invocations
                .push((workspace_root.to_path_buf(), check.clone()));
            Ok(*self.results.get(check).unwrap_or(&true))
        }
    }

    #[test]
    fn workspace_package_extraction_keeps_only_workspace_members() {
        let metadata = metadata_with_packages(
            vec![
                pkg("beta", "beta 0.1.0 (path+file:///tmp/beta)", &[], None),
                pkg("alpha", "alpha 0.1.0 (path+file:///tmp/alpha)", &[], None),
                pkg("gamma", "gamma 0.1.0 (path+file:///tmp/gamma)", &[], None),
            ],
            &[
                "beta 0.1.0 (path+file:///tmp/beta)",
                "alpha 0.1.0 (path+file:///tmp/alpha)",
            ],
        );

        let packages = workspace_packages(&metadata);

        assert_eq!(
            packages
                .iter()
                .map(|pkg| pkg.name.as_str())
                .collect::<Vec<_>>(),
            vec!["alpha", "beta"]
        );
    }

    #[test]
    fn parses_valid_clippy_csv() {
        let packages =
            parse_clippy_crates_csv("package\nalpha\nbeta\n", &known_packages()).unwrap();

        assert_eq!(packages, vec!["alpha".to_string(), "beta".to_string()]);
    }

    #[test]
    fn parses_clippy_csv_with_blank_lines() {
        let packages =
            parse_clippy_crates_csv("\npackage\n\nalpha\n\nbeta\n", &known_packages()).unwrap();

        assert_eq!(packages, vec!["alpha".to_string(), "beta".to_string()]);
    }

    #[test]
    fn rejects_empty_clippy_csv() {
        let err = parse_clippy_crates_csv("", &known_packages()).unwrap_err();

        assert!(err.to_string().contains("clippy crate csv is empty"));
    }

    #[test]
    fn rejects_invalid_clippy_csv_header() {
        let err = parse_clippy_crates_csv("crate\nalpha\n", &known_packages()).unwrap_err();

        assert!(err.to_string().contains("invalid header"));
    }

    #[test]
    fn rejects_unknown_clippy_csv_package() {
        let err = parse_clippy_crates_csv("package\nunknown\n", &known_packages()).unwrap_err();

        assert!(
            err.to_string()
                .contains("unknown workspace package `unknown`")
        );
    }

    #[test]
    fn rejects_duplicate_clippy_csv_package() {
        let err =
            parse_clippy_crates_csv("package\nalpha\nalpha\n", &known_packages()).unwrap_err();

        assert!(err.to_string().contains("duplicate package `alpha`"));
    }

    #[test]
    fn all_mode_selects_every_workspace_package() {
        let packages = vec![
            pkg("alpha", "alpha 0.1.0 (path+file:///tmp/alpha)", &[], None),
            pkg("beta", "beta 0.1.0 (path+file:///tmp/beta)", &[], None),
        ];
        let resolved =
            resolve_requested_packages(&args(true, &[]), Path::new("/tmp/ws"), &packages).unwrap();

        assert_eq!(
            resolved
                .iter()
                .map(|pkg| pkg.name.as_str())
                .collect::<Vec<_>>(),
            vec!["alpha", "beta"]
        );
    }

    #[test]
    fn package_selection_overrides_whitelist_file() {
        let packages = vec![
            pkg("alpha", "alpha 0.1.0 (path+file:///tmp/alpha)", &[], None),
            pkg("beta", "beta 0.1.0 (path+file:///tmp/beta)", &[], None),
        ];
        let resolved =
            resolve_requested_packages(&args(false, &["beta"]), Path::new("/tmp/ws"), &packages)
                .unwrap();

        assert_eq!(
            resolved
                .iter()
                .map(|pkg| pkg.name.as_str())
                .collect::<Vec<_>>(),
            vec!["beta"]
        );
    }

    #[test]
    fn duplicate_explicit_packages_are_rejected() {
        let known = known_packages();
        let err =
            validate_requested_packages(&["alpha".into(), "alpha".into()], &known).unwrap_err();

        assert!(
            err.to_string()
                .contains("duplicate workspace package `alpha`")
        );
    }

    #[test]
    fn feature_expansion_ignores_default() {
        let packages = vec![pkg(
            "alpha",
            "alpha 0.1.0 (path+file:///tmp/alpha)",
            &[("default", &["feat-a"]), ("feat-b", &[]), ("feat-a", &[])],
            None,
        )];

        let checks = expand_clippy_checks(&packages);

        assert_eq!(
            checks,
            vec![
                ClippyCheck {
                    package: "alpha".into(),
                    kind: ClippyCheckKind::Base,
                    target: None,
                },
                ClippyCheck {
                    package: "alpha".into(),
                    kind: ClippyCheckKind::Feature("feat-a".into()),
                    target: None,
                },
                ClippyCheck {
                    package: "alpha".into(),
                    kind: ClippyCheckKind::Feature("feat-b".into()),
                    target: None,
                },
            ]
        );
    }

    #[test]
    fn feature_expansion_is_deterministic() {
        let packages = vec![
            pkg(
                "beta",
                "beta 0.1.0 (path+file:///tmp/beta)",
                &[("zeta", &[]), ("alpha", &[])],
                None,
            ),
            pkg(
                "alpha",
                "alpha 0.1.0 (path+file:///tmp/alpha)",
                &[("middle", &[]), ("default", &[])],
                None,
            ),
        ];

        let checks = expand_clippy_checks(&packages);

        assert_eq!(
            checks
                .into_iter()
                .map(|check| check.label())
                .collect::<Vec<_>>(),
            vec![
                "beta (base)",
                "beta (feature: alpha)",
                "beta (feature: zeta)",
                "alpha (base)",
                "alpha (feature: middle)",
            ]
        );
    }

    #[test]
    fn package_without_features_yields_only_base_check() {
        let checks = expand_clippy_checks(&[pkg(
            "alpha",
            "alpha 0.1.0 (path+file:///tmp/alpha)",
            &[],
            None,
        )]);

        assert_eq!(
            checks,
            vec![ClippyCheck {
                package: "alpha".into(),
                kind: ClippyCheckKind::Base,
                target: None,
            }]
        );
    }

    #[test]
    fn package_with_features_yields_base_plus_each_feature() {
        let checks = expand_clippy_checks(&[pkg(
            "alpha",
            "alpha 0.1.0 (path+file:///tmp/alpha)",
            &[("b", &[]), ("a", &[])],
            None,
        )]);

        assert_eq!(checks.len(), 3);
        assert_eq!(
            checks[0].cargo_args(),
            vec!["clippy", "-p", "alpha", "--", "-D", "warnings"]
        );
        assert_eq!(
            checks[1].cargo_args(),
            vec![
                "clippy",
                "-p",
                "alpha",
                "--no-default-features",
                "--features",
                "a",
                "--",
                "-D",
                "warnings",
            ]
        );
        assert_eq!(
            checks[2].cargo_args(),
            vec![
                "clippy",
                "-p",
                "alpha",
                "--no-default-features",
                "--features",
                "b",
                "--",
                "-D",
                "warnings",
            ]
        );
    }

    #[test]
    fn docs_rs_targets_expand_base_and_feature_checks() {
        let checks = expand_clippy_checks(&[pkg(
            "alpha",
            "alpha 0.1.0 (path+file:///tmp/alpha)",
            &[("b", &[]), ("a", &[])],
            Some(&["riscv64gc-unknown-none-elf"]),
        )]);

        assert_eq!(checks.len(), 3);
        assert_eq!(
            checks[0].cargo_args(),
            vec![
                "clippy",
                "-p",
                "alpha",
                "--target",
                "riscv64gc-unknown-none-elf",
                "--",
                "-D",
                "warnings",
            ]
        );
        assert_eq!(
            checks[1].cargo_args(),
            vec![
                "clippy",
                "-p",
                "alpha",
                "--no-default-features",
                "--features",
                "a",
                "--target",
                "riscv64gc-unknown-none-elf",
                "--",
                "-D",
                "warnings",
            ]
        );
        assert_eq!(
            checks[2].label(),
            "alpha (feature: b, target: riscv64gc-unknown-none-elf)"
        );
    }

    #[test]
    fn docs_rs_targets_are_sorted_and_deduplicated() {
        let checks = expand_clippy_checks(&[pkg(
            "alpha",
            "alpha 0.1.0 (path+file:///tmp/alpha)",
            &[("feat", &[])],
            Some(&[
                "x86_64-unknown-none",
                "aarch64-unknown-none-softfloat",
                "x86_64-unknown-none",
            ]),
        )]);

        assert_eq!(
            checks
                .into_iter()
                .map(|check| check.label())
                .collect::<Vec<_>>(),
            vec![
                "alpha (base, target: aarch64-unknown-none-softfloat)",
                "alpha (feature: feat, target: aarch64-unknown-none-softfloat)",
                "alpha (base, target: x86_64-unknown-none)",
                "alpha (feature: feat, target: x86_64-unknown-none)",
            ]
        );
    }

    #[test]
    fn empty_docs_rs_targets_fall_back_to_host_clippy() {
        let package = pkg(
            "alpha",
            "alpha 0.1.0 (path+file:///tmp/alpha)",
            &[],
            Some(&[]),
        );

        assert!(docs_rs_targets(&package).is_empty());
        assert_eq!(
            expand_clippy_checks(&[package])[0].cargo_args(),
            vec!["clippy", "-p", "alpha", "--", "-D", "warnings"]
        );
    }

    #[test]
    fn package_failures_keep_the_package_out_of_the_pass_list() {
        let root = PathBuf::from("/tmp/workspace");
        let checks = vec![
            ClippyCheck {
                package: "alpha".into(),
                kind: ClippyCheckKind::Base,
                target: None,
            },
            ClippyCheck {
                package: "alpha".into(),
                kind: ClippyCheckKind::Feature("feat-a".into()),
                target: None,
            },
            ClippyCheck {
                package: "beta".into(),
                kind: ClippyCheckKind::Base,
                target: None,
            },
        ];
        let mut runner = FakeCargoRunner::new(&[
            (checks[0].clone(), true),
            (checks[1].clone(), false),
            (checks[2].clone(), true),
        ]);

        let report = run_clippy_checks(&mut runner, &root, &checks).unwrap();

        assert_eq!(report.passed_packages(), vec!["beta"]);
        assert_eq!(report.failed_packages(), vec!["alpha"]);
        assert_eq!(
            report
                .packages
                .iter()
                .find(|package| package.package == "alpha")
                .unwrap()
                .failed_checks,
            vec!["alpha (feature: feat-a)".to_string()]
        );
        assert_eq!(
            runner.invocations,
            vec![
                (root.clone(), checks[0].clone()),
                (root.clone(), checks[1].clone()),
                (root, checks[2].clone()),
            ]
        );
    }

    #[test]
    fn report_exposes_csv_ready_passing_packages_for_mixed_runs() {
        let report = ClippyRunReport {
            total_checks: 3,
            passed_checks: 2,
            packages: vec![
                PackageRunReport {
                    package: "alpha".into(),
                    total_checks: 2,
                    failed_checks: vec!["alpha (feature: feat-a)".into()],
                },
                PackageRunReport {
                    package: "beta".into(),
                    total_checks: 1,
                    failed_checks: Vec::new(),
                },
            ],
        };

        assert_eq!(report.failed_packages(), vec!["alpha"]);
        assert_eq!(report.passed_packages(), vec!["beta"]);
        assert_eq!(report.passing_packages_csv(), "package\nbeta\n");
    }
}
