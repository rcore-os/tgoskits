#![cfg_attr(not(any(windows, unix)), no_main)]
#![cfg_attr(not(any(windows, unix)), no_std)]
#![cfg(any(windows, unix))]

#[macro_use]
extern crate anyhow;

use std::{collections::HashSet, fs, path::Path, process::Command};

use anyhow::{Context, Result};
use cargo_metadata::{Metadata, MetadataCommand};
use clap::{Parser, Subcommand};

mod axvisor;

const STD_CRATES_CSV: &str = "scripts/test/std_crates.csv";

const AXVISOR_TARGETS: &[&str] = &[
    "x86_64-unknown-none",
    "riscv64gc-unknown-none-elf",
    "aarch64-unknown-none-softfloat",
];

const STARRY_TARGETS: &[&str] = &[
    "x86_64-unknown-none",
    "riscv64gc-unknown-none-elf",
    "aarch64-unknown-none-softfloat",
    "loongarch64-unknown-none-softfloat",
];

fn supported_targets(os: &str) -> &'static [&'static str] {
    match os {
        "axvisor" => AXVISOR_TARGETS,
        "starry" => STARRY_TARGETS,
        _ => &[],
    }
}

#[derive(Parser)]
#[command(name = "tg-xtask")]
#[command(about = "Workspace maintenance tasks")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Test {
        #[command(subcommand)]
        command: TestCommand,
    },
}

#[derive(Subcommand)]
enum TestCommand {
    Std,
    Axvisor {
        /// Target triple for cross-compilation
        #[arg(long)]
        target: String,
    },
    Starry {
        /// Target triple for cross-compilation
        #[arg(long)]
        target: String,
    },
}

trait CargoRunner {
    fn run_test(&mut self, workspace_root: &Path, package: &str) -> Result<bool>;
}

struct ProcessCargoRunner;

impl CargoRunner for ProcessCargoRunner {
    fn run_test(&mut self, workspace_root: &Path, package: &str) -> Result<bool> {
        let args = cargo_test_args(package);
        let status = Command::new("cargo")
            .current_dir(workspace_root)
            .args(&args)
            .status()
            .with_context(|| format!("failed to spawn `cargo {}`", args.join(" ")))?;
        Ok(status.success())
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Test {
            command: TestCommand::Std,
        } => run_std_test_command(),
        Commands::Test {
            command: TestCommand::Axvisor { target },
        } => run_target_test_command("axvisor", &target),
        Commands::Test {
            command: TestCommand::Starry { target },
        } => run_target_test_command("starry", &target),
    }
}

fn run_std_test_command() -> Result<()> {
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
    let workspace_members: HashSet<_> = metadata.workspace_members.iter().cloned().collect();
    metadata
        .packages
        .iter()
        .filter(|pkg| workspace_members.contains(&pkg.id))
        .map(|pkg| pkg.name.to_string())
        .collect()
}

fn load_std_crates(csv_path: &Path, known_packages: &HashSet<String>) -> Result<Vec<String>> {
    let contents = fs::read_to_string(csv_path)
        .with_context(|| format!("failed to read {}", csv_path.display()))?;
    parse_std_crates_csv(&contents, known_packages)
}

fn parse_std_crates_csv(contents: &str, known_packages: &HashSet<String>) -> Result<Vec<String>> {
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
) -> Result<Vec<String>> {
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

fn run_target_test_command(os: &str, target: &str) -> Result<()> {
    let supported = supported_targets(os);

    // 验证 target 是否在支持的列表中
    if !supported.contains(&target) {
        bail!(
            "unsupported target `{}` for {}. Supported targets are: {}",
            target,
            os,
            supported.join(", ")
        );
    }

    let metadata = MetadataCommand::new()
        .no_deps()
        .exec()
        .context("failed to load cargo metadata")?;
    let _workspace_root = metadata.workspace_root.clone().into_std_path_buf();

    println!("running {} tests for target: {}", os, target);

    match os {
        "axvisor" => axvisor::run_test(target)?,
        "starry" => {
            // starry 的测试实现占位
            println!(
                "  (test implementation placeholder for {} on {})",
                os, target
            );
        }
        _ => unreachable!(), // 之前已经验证过了
    }

    println!("{} test passed", os);
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{collections::VecDeque, path::PathBuf};

    use super::*;

    struct FakeCargoRunner {
        statuses: VecDeque<bool>,
        calls: Vec<(PathBuf, String)>,
    }

    impl FakeCargoRunner {
        fn new(statuses: Vec<bool>) -> Self {
            Self {
                statuses: statuses.into(),
                calls: Vec::new(),
            }
        }
    }

    impl CargoRunner for FakeCargoRunner {
        fn run_test(&mut self, workspace_root: &Path, package: &str) -> Result<bool> {
            self.calls
                .push((workspace_root.to_path_buf(), package.to_owned()));
            self.statuses
                .pop_front()
                .context("missing fake runner status")
        }
    }

    fn known_packages(packages: &[&str]) -> HashSet<String> {
        packages.iter().map(|pkg| (*pkg).to_owned()).collect()
    }

    #[test]
    fn parses_valid_csv_and_ignores_empty_lines() {
        let csv = "package\n\naxhal\naxlog\n";
        let parsed = parse_std_crates_csv(csv, &known_packages(&["axhal", "axlog"])).unwrap();
        assert_eq!(parsed, vec!["axhal", "axlog"]);
    }

    #[test]
    fn rejects_duplicate_packages() {
        let csv = "package\naxhal\naxhal\n";
        let error = parse_std_crates_csv(csv, &known_packages(&["axhal"])).unwrap_err();
        assert!(error.to_string().contains("duplicate package `axhal`"));
    }

    #[test]
    fn rejects_unknown_packages() {
        let csv = "package\naxhal\naxstd\n";
        let error = parse_std_crates_csv(csv, &known_packages(&["axhal"])).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("unknown workspace package `axstd`")
        );
    }

    #[test]
    fn builds_expected_cargo_test_args() {
        assert_eq!(cargo_test_args("axhal"), vec!["test", "-p", "axhal"]);
    }

    #[test]
    fn continues_after_failures_and_collects_them() {
        let workspace_root = PathBuf::from("/tmp/workspace");
        let packages = vec!["axhal".to_owned(), "axstd".to_owned(), "axlog".to_owned()];
        let mut runner = FakeCargoRunner::new(vec![true, false, true]);

        let failed = run_std_tests(&mut runner, &workspace_root, &packages).unwrap();

        assert_eq!(failed, vec!["axstd"]);
        assert_eq!(
            runner.calls,
            vec![
                (workspace_root.clone(), "axhal".to_owned()),
                (workspace_root.clone(), "axstd".to_owned()),
                (workspace_root, "axlog".to_owned()),
            ]
        );
    }

    #[test]
    fn supported_targets_returns_axvisor_targets() {
        let targets = supported_targets("axvisor");
        assert_eq!(
            targets,
            &[
                "x86_64-unknown-none",
                "riscv64gc-unknown-none-elf",
                "aarch64-unknown-none-softfloat",
            ]
        );
    }

    #[test]
    fn supported_targets_returns_starry_targets() {
        let targets = supported_targets("starry");
        assert_eq!(
            targets,
            &[
                "x86_64-unknown-none",
                "riscv64gc-unknown-none-elf",
                "aarch64-unknown-none-softfloat",
                "loongarch64-unknown-none-softfloat",
            ]
        );
    }

    #[test]
    fn supported_targets_returns_empty_for_unknown_os() {
        let targets = supported_targets("unknown");
        assert!(targets.is_empty());
    }

    #[test]
    fn axvisor_contains_expected_targets() {
        let targets = supported_targets("axvisor");
        assert!(targets.contains(&"x86_64-unknown-none"));
        assert!(targets.contains(&"riscv64gc-unknown-none-elf"));
        assert!(targets.contains(&"aarch64-unknown-none-softfloat"));
    }

    #[test]
    fn starry_contains_expected_targets() {
        let targets = supported_targets("starry");
        assert!(targets.contains(&"x86_64-unknown-none"));
        assert!(targets.contains(&"riscv64gc-unknown-none-elf"));
        assert!(targets.contains(&"aarch64-unknown-none-softfloat"));
        assert!(targets.contains(&"loongarch64-unknown-none-softfloat"));
    }

    #[test]
    fn starry_supports_loongarch() {
        let targets = supported_targets("starry");
        let axvisor_targets = supported_targets("axvisor");
        // starry 应该支持 loongarch，但 axvisor 不支持
        assert!(targets.contains(&"loongarch64-unknown-none-softfloat"));
        assert!(!axvisor_targets.contains(&"loongarch64-unknown-none-softfloat"));
    }
}
