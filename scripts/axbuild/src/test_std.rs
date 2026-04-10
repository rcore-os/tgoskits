use std::{collections::HashSet, fs, path::Path, process::Command};

use anyhow::Context;
use cargo_metadata::{Metadata, MetadataCommand};

const STD_CRATES_CSV: &str = "scripts/test/std_crates.csv";

pub(crate) fn run_std_test_command() -> anyhow::Result<()> {
    let metadata = MetadataCommand::new()
        .no_deps()
        .exec()
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

fn cargo_test_args(package: &str) -> Vec<String> {
    vec!["test".into(), "-p".into(), package.into()]
}

fn run_std_tests<R: CargoRunner>(
    runner: &mut R,
    workspace_root: &Path,
    packages: &[String],
) -> anyhow::Result<Vec<String>> {
    let mut failed = Vec::new();

    for (index, package) in packages.iter().enumerate() {
        println!(
            "[{}/{}] cargo {}",
            index + 1,
            packages.len(),
            cargo_test_args(package).join(" ")
        );
        if runner.run_test(workspace_root, package)? {
            println!("ok: {}", package);
        } else {
            eprintln!("failed: {}", package);
            failed.push(package.clone());
        }
    }

    Ok(failed)
}

trait CargoRunner {
    fn run_test(&mut self, workspace_root: &Path, package: &str) -> anyhow::Result<bool>;
}

struct ProcessCargoRunner;

impl CargoRunner for ProcessCargoRunner {
    fn run_test(&mut self, workspace_root: &Path, package: &str) -> anyhow::Result<bool> {
        let args = cargo_test_args(package);
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
        fn run_test(&mut self, workspace_root: &Path, package: &str) -> anyhow::Result<bool> {
            self.invocations
                .push((workspace_root.to_path_buf(), package.to_string()));
            Ok(*self.results.get(package).unwrap_or(&true))
        }
    }

    #[test]
    fn parses_valid_std_csv() {
        let packages = parse_std_crates_csv("package\naxfeat\naxhal\n", &known_packages()).unwrap();

        assert_eq!(packages, vec!["ax-feat".to_string(), "ax-hal".to_string()]);
    }

    #[test]
    fn parses_std_csv_with_blank_lines() {
        let packages =
            parse_std_crates_csv("\npackage\n\naxfeat\n\naxhal\n", &known_packages()).unwrap();

        assert_eq!(packages, vec!["ax-feat".to_string(), "ax-hal".to_string()]);
    }

    #[test]
    fn rejects_empty_std_csv() {
        let err = parse_std_crates_csv("", &known_packages()).unwrap_err();

        assert!(err.to_string().contains("std crate csv is empty"));
    }

    #[test]
    fn rejects_invalid_header() {
        let err = parse_std_crates_csv("crate\naxfeat\n", &known_packages()).unwrap_err();

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
        let err = parse_std_crates_csv("package\naxfeat\naxfeat\n", &known_packages()).unwrap_err();

        assert!(err.to_string().contains("duplicate package `ax-feat`"));
    }

    #[test]
    fn workspace_package_name_extraction_reads_current_workspace() {
        let metadata = MetadataCommand::new().no_deps().exec().unwrap();
        let names = workspace_package_names(&metadata);

        assert!(names.contains("axbuild"));
        assert!(names.contains("tg-xtask"));
    }

    #[test]
    fn std_test_runner_collects_all_failures() {
        let root = PathBuf::from("/tmp/workspace");
        let packages = vec![
            "ax-feat".to_string(),
            "ax-hal".to_string(),
            "starry-process".to_string(),
        ];
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
        let packages = vec!["ax-feat".to_string(), "ax-hal".to_string()];
        let mut runner = FakeCargoRunner::new(&[("ax-feat", true), ("ax-hal", true)]);

        let failed = run_std_tests(&mut runner, &root, &packages).unwrap();

        assert!(failed.is_empty());
    }
}
