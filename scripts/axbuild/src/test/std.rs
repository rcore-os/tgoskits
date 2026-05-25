use std::{collections::HashSet, fs, path::Path};

use anyhow::{Context, bail};
use cargo_metadata::Metadata;

use crate::support::process::run_cargo_status;

const STD_CRATES_CSV: &str = "scripts/test/std_crates.csv";

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

/// A package entry from the std-crates CSV, with optional feature flags.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TestPackage {
    name: String,
    /// Feature names to pass via `--features`. Empty means no `--features` flag.
    features: Vec<String>,
}

fn load_std_crates(
    csv_path: &Path,
    known_packages: &HashSet<String>,
) -> anyhow::Result<Vec<TestPackage>> {
    let contents = fs::read_to_string(csv_path)
        .with_context(|| format!("failed to read {}", csv_path.display()))?;
    parse_std_crates_csv(&contents, known_packages)
}

/// Parse the std-crates CSV.
///
/// Each data line may be either `package` or `package,feat1,feat2,...`.  The
/// first field must be a known workspace package name; any remaining
/// comma-separated fields are treated as Cargo feature names passed via
/// `--features` when the test is run.
fn parse_std_crates_csv(
    contents: &str,
    known_packages: &HashSet<String>,
) -> anyhow::Result<Vec<TestPackage>> {
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
    for (line_no, line) in lines {
        let mut fields = line.splitn(2, ',');
        let package = fields.next().unwrap_or("").trim();
        let features: Vec<String> = fields
            .next()
            .unwrap_or("")
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect();

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
        packages.push(TestPackage {
            name: package.to_owned(),
            features,
        });
    }

    Ok(packages)
}

fn cargo_test_args(pkg: &TestPackage) -> Vec<String> {
    let mut args = vec!["test".into(), "-p".into(), pkg.name.clone()];
    if !pkg.features.is_empty() {
        args.push("--features".into());
        args.push(pkg.features.join(","));
    }
    args
}

fn run_std_tests<R: CargoRunner>(
    runner: &mut R,
    workspace_root: &Path,
    packages: &[TestPackage],
) -> anyhow::Result<Vec<String>> {
    let mut failed = Vec::new();

    for (index, pkg) in packages.iter().enumerate() {
        println!(
            "[{}/{}] cargo {}",
            index + 1,
            packages.len(),
            cargo_test_args(pkg).join(" ")
        );
        if runner.run_test(workspace_root, pkg)? {
            println!("ok: {}", pkg.name);
        } else {
            eprintln!("failed: {}", pkg.name);
            failed.push(pkg.name.clone());
        }
    }

    Ok(failed)
}

trait CargoRunner {
    fn run_test(&mut self, workspace_root: &Path, pkg: &TestPackage) -> anyhow::Result<bool>;
}

struct ProcessCargoRunner;

impl CargoRunner for ProcessCargoRunner {
    fn run_test(&mut self, workspace_root: &Path, pkg: &TestPackage) -> anyhow::Result<bool> {
        let args = cargo_test_args(pkg);
        run_cargo_status(workspace_root, &args)
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
        invocations: Vec<(PathBuf, String)>,
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
        fn run_test(&mut self, workspace_root: &Path, pkg: &TestPackage) -> anyhow::Result<bool> {
            self.invocations
                .push((workspace_root.to_path_buf(), pkg.name.clone()));
            Ok(*self.results.get(&pkg.name).unwrap_or(&true))
        }
    }

    fn pkg(name: &str) -> TestPackage {
        TestPackage {
            name: name.to_string(),
            features: vec![],
        }
    }

    fn pkg_with_features(name: &str, features: &[&str]) -> TestPackage {
        TestPackage {
            name: name.to_string(),
            features: features.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn parses_valid_std_csv() {
        let packages =
            parse_std_crates_csv("package\nax-feat\nax-hal\n", &known_packages()).unwrap();

        assert_eq!(packages, vec![pkg("ax-feat"), pkg("ax-hal")]);
    }

    #[test]
    fn parses_std_csv_with_blank_lines() {
        let packages =
            parse_std_crates_csv("\npackage\n\nax-feat\n\nax-hal\n", &known_packages()).unwrap();

        assert_eq!(packages, vec![pkg("ax-feat"), pkg("ax-hal")]);
    }

    #[test]
    fn parses_std_csv_with_features() {
        let packages = parse_std_crates_csv(
            "package\nax-feat,feat-a,feat-b\nax-hal\n",
            &known_packages(),
        )
        .unwrap();

        assert_eq!(
            packages,
            vec![
                pkg_with_features("ax-feat", &["feat-a", "feat-b"]),
                pkg("ax-hal")
            ]
        );
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
    fn std_test_runner_collects_all_failures() {
        let root = PathBuf::from("/tmp/workspace");
        let packages = vec![pkg("ax-feat"), pkg("ax-hal"), pkg("starry-process")];
        let mut runner = FakeCargoRunner::new(&[
            ("ax-feat", true),
            ("ax-hal", false),
            ("starry-process", false),
        ]);

        let failed = run_std_tests(&mut runner, &root, &packages).unwrap();

        assert_eq!(
            failed,
            vec!["ax-hal".to_string(), "starry-process".to_string()]
        );
        assert_eq!(
            runner.invocations,
            vec![
                (root.clone(), "ax-feat".to_string()),
                (root.clone(), "ax-hal".to_string()),
                (root, "starry-process".to_string()),
            ]
        );
    }

    #[test]
    fn std_test_runner_returns_empty_failures_when_all_pass() {
        let root = PathBuf::from("/tmp/workspace");
        let packages = vec![pkg("ax-feat"), pkg("ax-hal")];
        let mut runner = FakeCargoRunner::new(&[("ax-feat", true), ("ax-hal", true)]);

        let failed = run_std_tests(&mut runner, &root, &packages).unwrap();

        assert!(failed.is_empty());
    }

    #[test]
    fn cargo_test_args_without_features() {
        let p = pkg("ax-hal");
        assert_eq!(cargo_test_args(&p), vec!["test", "-p", "ax-hal"]);
    }

    #[test]
    fn cargo_test_args_with_features() {
        let p = pkg_with_features("ax-task", &["multitask", "sched-fifo", "test"]);
        assert_eq!(
            cargo_test_args(&p),
            vec![
                "test",
                "-p",
                "ax-task",
                "--features",
                "multitask,sched-fifo,test"
            ]
        );
    }
}
