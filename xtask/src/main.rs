#![cfg_attr(not(any(windows, unix)), no_main)]
#![cfg_attr(not(any(windows, unix)), no_std)]
#![cfg(any(windows, unix))]

#[macro_use]
extern crate anyhow;

use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result};
use axbuild::arceos::{Arch, PlatformResolver, RunScope};
use cargo_metadata::{Metadata, MetadataCommand};
use clap::{Parser, Subcommand};

mod arceos;
mod axvisor;
mod starry;

const STD_CRATES_CSV: &str = "scripts/test/std_crates.csv";

const STARRY_TARGETS: &[&str] = &[
    "x86_64-unknown-none",
    "riscv64gc-unknown-none-elf",
    "aarch64-unknown-none-softfloat",
    "loongarch64-unknown-none-softfloat",
];

const ARCEOS_TARGETS: &[&str] = &[
    "x86_64-unknown-none",
    "riscv64gc-unknown-none-elf",
    "aarch64-unknown-none-softfloat",
    "loongarch64-unknown-none-softfloat",
];

fn supported_targets(os: &str) -> &'static [&'static str] {
    match os {
        "starry" => STARRY_TARGETS,
        "arceos" => ARCEOS_TARGETS,
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
    /// ArceOS build commands
    Arceos {
        #[command(subcommand)]
        command: arceos::ArceosCommand,
    },
    /// StarryOS build commands
    Starry {
        #[command(subcommand)]
        command: starry::StarryCommand,
    },
}

#[derive(Subcommand)]
enum TestCommand {
    Std,
    Axvisor {
        /// Target triple for cross-compilation
        #[arg(short, long)]
        target: Option<String>,
    },
    Starry {
        /// Target triple for cross-compilation
        #[arg(long)]
        target: String,
    },
    Arceos {
        /// Target triple for cross-compilation
        #[arg(long)]
        target: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ArceosTestPackage {
    name: String,
    crate_dir: PathBuf,
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

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Test {
            command: TestCommand::Std,
        } => run_std_test_command(),
        Commands::Test {
            command: TestCommand::Axvisor { target },
        } => axvisor::run_test_qemu(target).await,
        Commands::Test {
            command: TestCommand::Starry { target },
        } => run_target_test_command("starry", &target).await,
        Commands::Test {
            command: TestCommand::Arceos { target },
        } => run_arceos_test_command(target.as_deref()).await,
        Commands::Arceos { command } => command.run().await,
        Commands::Starry { command } => command.run().await,
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

async fn run_target_test_command(os: &str, target: &str) -> Result<()> {
    println!("running {} tests for target: {}", os, target);

    match os {
        "starry" => starry::run_test(target).await?,
        _ => unreachable!(), // 之前已经验证过了
    }

    println!("{} test passed", os);
    Ok(())
}

async fn run_arceos_test_command(target: Option<&str>) -> Result<()> {
    let metadata = MetadataCommand::new()
        .no_deps()
        .exec()
        .context("failed to load cargo metadata")?;
    let packages = discover_arceos_test_packages(&metadata);
    if packages.is_empty() {
        println!("no arceos test packages found under test-suit/arceos");
        return Ok(());
    }

    let arch = target.map(parse_arceos_target).transpose()?;
    let selected_arch = arceos_test_arch(arch);

    println!(
        "running arceos tests for {} package(s){}",
        packages.len(),
        target
            .map(|t| format!(" on target: {}", t))
            .unwrap_or_default()
    );

    for (index, package) in packages.iter().enumerate() {
        println!(
            "[{}/{}] arceos run -p {}{}",
            index + 1,
            packages.len(),
            package.name,
            target
                .map(|t| format!(" --target {}", t))
                .unwrap_or_default()
        );
        let run_args = arceos::RunArgs {
            build: arceos::BuildArgs {
                arch: arch.map(|value| value.to_string()),
                package: package.name.clone(),
                platform: arch.map(|value| PlatformResolver::resolve_default_platform_name(&value)),
                release: true,
                features: None,
                smp: None,
                plat_dyn: Some(matches!(selected_arch, Arch::AArch64)),
            },
            blk: false,
            disk_img: None,
            net: false,
            net_dev: None,
            graphic: false,
            accel: false,
        };
        arceos::test_with_arg_in_scope(run_args, RunScope::PackageRoot)
            .await
            .with_context(|| format!("arceos test failed for package `{}`", package.name))?;
        println!("ok: {}", package.name);
    }

    println!("all arceos tests passed");
    Ok(())
}

fn discover_arceos_test_packages(metadata: &Metadata) -> Vec<ArceosTestPackage> {
    let workspace_root = metadata.workspace_root.clone().into_std_path_buf();
    let workspace_members: HashSet<_> = metadata.workspace_members.iter().cloned().collect();
    let packages = metadata
        .packages
        .iter()
        .filter(|pkg| workspace_members.contains(&pkg.id))
        .map(|pkg| {
            (
                pkg.name.to_string(),
                pkg.manifest_path.clone().into_std_path_buf(),
            )
        });
    collect_arceos_test_packages(&workspace_root, packages)
}

fn collect_arceos_test_packages<I>(workspace_root: &Path, packages: I) -> Vec<ArceosTestPackage>
where
    I: IntoIterator<Item = (String, PathBuf)>,
{
    let arceos_test_root = workspace_root.join("test-suit/arceos");
    let mut selected = packages
        .into_iter()
        .filter_map(|(name, manifest_path)| {
            let crate_dir = manifest_path.parent()?.to_path_buf();
            if crate_dir.starts_with(&arceos_test_root) {
                Some(ArceosTestPackage { name, crate_dir })
            } else {
                None
            }
        })
        .collect::<Vec<_>>();

    selected.sort_by(|a, b| {
        a.crate_dir
            .cmp(&b.crate_dir)
            .then_with(|| a.name.cmp(&b.name))
    });
    selected
}

fn parse_arceos_target(target: &str) -> Result<Arch> {
    match target {
        "x86_64-unknown-none" => Ok(Arch::X86_64),
        "aarch64-unknown-none-softfloat" => Ok(Arch::AArch64),
        "riscv64gc-unknown-none-elf" => Ok(Arch::RiscV64),
        "loongarch64-unknown-none-softfloat" => Ok(Arch::LoongArch64),
        _ => bail!(
            "unsupported target `{}` for arceos. Supported targets are: {}",
            target,
            supported_targets("arceos").join(", ")
        ),
    }
}

fn arceos_test_arch(target_arch: Option<Arch>) -> Arch {
    target_arch.unwrap_or_default()
}
