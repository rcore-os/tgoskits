use std::{
    collections::{HashMap, HashSet},
    fs,
    path::Path,
    process::Command,
};

use anyhow::Context;
use cargo_metadata::{Metadata, Package};

const STD_CRATES_CSV: &str = "scripts/test/std_crates.csv";

pub(crate) fn run_std_test_command() -> anyhow::Result<()> {
    let workspace_manifest = crate::context::workspace_manifest_path()?;
    let metadata = crate::context::workspace_metadata_root_manifest(&workspace_manifest)
        .context("failed to load cargo metadata")?;
    let workspace_root = metadata.workspace_root.clone().into_std_path_buf();
    let known_packages = workspace_package_names(&metadata);
    let csv_path = workspace_root.join(STD_CRATES_CSV);
    let packages = load_std_crates(&csv_path, &known_packages)?;
    let requested_features = crate::arceos::build::makefile_features_from_env();
    let invocations = build_std_test_invocations(&metadata, &packages, &requested_features)?;

    println!(
        "running std tests for {} package(s) from {}",
        invocations.len(),
        csv_path.display()
    );
    if !requested_features.is_empty() {
        println!(
            "requested Makefile features for std tests: {}",
            requested_features.join(",")
        );
    }

    let mut runner = ProcessCargoRunner;
    let failed = run_std_tests(&mut runner, &workspace_root, &invocations)?;

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

#[derive(Debug, Clone, PartialEq, Eq)]
struct StdTestInvocation {
    package: String,
    features: Vec<String>,
}

fn build_std_test_invocations(
    metadata: &Metadata,
    packages: &[String],
    requested_features: &[String],
) -> anyhow::Result<Vec<StdTestInvocation>> {
    let workspace_members: HashSet<_> = metadata.workspace_members.iter().cloned().collect();
    let package_lookup: HashMap<_, _> = metadata
        .packages
        .iter()
        .filter(|pkg| workspace_members.contains(&pkg.id))
        .map(|pkg| (pkg.name.as_str(), pkg))
        .collect();

    packages
        .iter()
        .map(|package| {
            let package_info = package_lookup
                .get(package.as_str())
                .copied()
                .ok_or_else(|| anyhow::anyhow!("workspace package `{package}` not found"))?;

            Ok(StdTestInvocation {
                package: package.clone(),
                features: resolve_std_test_features(package_info, requested_features),
            })
        })
        .collect()
}

fn resolve_std_test_features(package: &Package, requested_features: &[String]) -> Vec<String> {
    let mut resolved = Vec::new();
    let has_axstd = package_dep_matches(package, "ax-std");
    let has_axfeat = package_dep_matches(package, "ax-feat");
    let has_axlibc = package_dep_matches(package, "ax-libc");

    for feature in requested_features {
        let normalized = crate::arceos::build::normalize_legacy_feature_alias(feature);
        let mapped = if normalized.contains('/')
            || matches!(normalized.as_str(), "ax-std" | "ax-feat" | "ax-libc")
            || package.features.contains_key(&normalized)
        {
            normalized
        } else if has_axstd {
            format!("ax-std/{normalized}")
        } else if has_axfeat {
            format!("ax-feat/{normalized}")
        } else if has_axlibc {
            format!("ax-libc/{normalized}")
        } else {
            continue;
        };

        if !resolved.iter().any(|existing| existing == &mapped) {
            resolved.push(mapped);
        }
    }

    resolved
}

fn package_dep_matches(package: &Package, dep_name: &str) -> bool {
    package
        .dependencies
        .iter()
        .any(|dep| dep.name == dep_name || dep.rename.as_deref() == Some(dep_name))
}

fn cargo_test_args(invocation: &StdTestInvocation) -> Vec<String> {
    let mut args = vec!["test".into(), "-p".into(), invocation.package.clone()];
    if !invocation.features.is_empty() {
        args.push("--features".into());
        args.push(invocation.features.join(","));
    }
    args
}

fn run_std_tests<R: CargoRunner>(
    runner: &mut R,
    workspace_root: &Path,
    invocations: &[StdTestInvocation],
) -> anyhow::Result<Vec<String>> {
    let mut failed = Vec::new();

    for (index, invocation) in invocations.iter().enumerate() {
        let args = cargo_test_args(invocation);
        println!(
            "[{}/{}] cargo {}",
            index + 1,
            invocations.len(),
            args.join(" ")
        );
        if runner.run_test(workspace_root, invocation)? {
            println!("ok: {}", invocation.package);
        } else {
            eprintln!("failed: {}", invocation.package);
            failed.push(invocation.package.clone());
        }
    }

    Ok(failed)
}

trait CargoRunner {
    fn run_test(
        &mut self,
        workspace_root: &Path,
        invocation: &StdTestInvocation,
    ) -> anyhow::Result<bool>;
}

struct ProcessCargoRunner;

impl CargoRunner for ProcessCargoRunner {
    fn run_test(
        &mut self,
        workspace_root: &Path,
        invocation: &StdTestInvocation,
    ) -> anyhow::Result<bool> {
        let args = cargo_test_args(invocation);
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

    fn known_packages() -> HashSet<String> {
        HashSet::from([
            "ax-feat".to_string(),
            "ax-hal".to_string(),
            "starry-process".to_string(),
        ])
    }

    struct FakeCargoRunner {
        results: HashMap<String, bool>,
        invocations: Vec<(PathBuf, StdTestInvocation)>,
    }

    impl FakeCargoRunner {
        fn new(results: &[(&str, bool)]) -> Self {
            Self {
                results: results
                    .iter()
                    .map(|(name, ok)| ((*name).to_string(), *ok))
                    .collect(),
                invocations: Vec::new(),
            }
        }
    }

    impl CargoRunner for FakeCargoRunner {
        fn run_test(
            &mut self,
            workspace_root: &Path,
            invocation: &StdTestInvocation,
        ) -> anyhow::Result<bool> {
            self.invocations
                .push((workspace_root.to_path_buf(), invocation.clone()));
            Ok(*self.results.get(&invocation.package).unwrap_or(&true))
        }
    }

    #[test]
    fn parses_valid_std_csv() {
        let packages =
            parse_std_crates_csv("package\nax-feat\nax-hal\n", &known_packages()).unwrap();

        assert_eq!(packages, vec!["ax-feat".to_string(), "ax-hal".to_string()]);
    }

    #[test]
    fn parses_std_csv_with_blank_lines() {
        let packages =
            parse_std_crates_csv("\npackage\n\nax-feat\n\nax-hal\n", &known_packages()).unwrap();

        assert_eq!(packages, vec!["ax-feat".to_string(), "ax-hal".to_string()]);
    }

    #[test]
    fn rejects_empty_std_csv() {
        let err = parse_std_crates_csv("", &known_packages()).unwrap_err();

        assert!(err.to_string().contains("std crate csv is empty"));
    }

    #[test]
    fn rejects_invalid_header() {
        let err = parse_std_crates_csv("crate\nax-feat\n", &known_packages()).unwrap_err();

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
        let err =
            parse_std_crates_csv("package\nax-feat\nax-feat\n", &known_packages()).unwrap_err();

        assert!(err.to_string().contains("duplicate package `ax-feat`"));
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
    fn resolve_std_test_features_prefers_local_feature_when_present() {
        let metadata = cargo_metadata::MetadataCommand::new().exec().unwrap();
        let package = metadata
            .packages
            .iter()
            .find(|pkg| pkg.name == "ax-feat")
            .unwrap();

        assert_eq!(
            resolve_std_test_features(package, &[String::from("lockdep")]),
            vec![String::from("lockdep")]
        );
    }

    #[test]
    fn resolve_std_test_features_maps_to_axfeat_dependency_feature() {
        let metadata = cargo_metadata::MetadataCommand::new().exec().unwrap();
        let package = metadata
            .packages
            .iter()
            .find(|pkg| pkg.name == "ax-api")
            .unwrap();

        assert_eq!(
            resolve_std_test_features(package, &[String::from("lockdep")]),
            vec![String::from("ax-feat/lockdep")]
        );
    }

    #[test]
    fn resolve_std_test_features_skips_unsupported_feature_requests() {
        let metadata = cargo_metadata::MetadataCommand::new().exec().unwrap();
        let package = metadata
            .packages
            .iter()
            .find(|pkg| pkg.name == "ax-hal")
            .unwrap();

        assert!(resolve_std_test_features(package, &[String::from("lockdep")]).is_empty());
    }

    #[test]
    fn std_test_runner_collects_all_failures() {
        let root = PathBuf::from("/tmp/workspace");
        let invocations = vec![
            StdTestInvocation {
                package: "ax-feat".to_string(),
                features: vec![String::from("lockdep")],
            },
            StdTestInvocation {
                package: "ax-hal".to_string(),
                features: Vec::new(),
            },
            StdTestInvocation {
                package: "starry-process".to_string(),
                features: vec![String::from("ax-feat/lockdep")],
            },
        ];
        let mut runner = FakeCargoRunner::new(&[
            ("ax-feat", true),
            ("ax-hal", false),
            ("starry-process", false),
        ]);

        let failed = run_std_tests(&mut runner, &root, &invocations).unwrap();

        assert_eq!(
            failed,
            vec!["ax-hal".to_string(), "starry-process".to_string()]
        );
        assert_eq!(
            runner.invocations,
            vec![
                (root.clone(), invocations[0].clone()),
                (root.clone(), invocations[1].clone()),
                (root, invocations[2].clone()),
            ]
        );
    }

    #[test]
    fn std_test_runner_returns_empty_failures_when_all_pass() {
        let root = PathBuf::from("/tmp/workspace");
        let invocations = vec![
            StdTestInvocation {
                package: "ax-feat".to_string(),
                features: vec![String::from("lockdep")],
            },
            StdTestInvocation {
                package: "ax-hal".to_string(),
                features: Vec::new(),
            },
        ];
        let mut runner = FakeCargoRunner::new(&[("ax-feat", true), ("ax-hal", true)]);

        let failed = run_std_tests(&mut runner, &root, &invocations).unwrap();

        assert!(failed.is_empty());
    }
}
