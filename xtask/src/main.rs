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
use axbuild::arceos::{Arch, PlatformResolver, context::AxContext};
use cargo_metadata::{Metadata, MetadataCommand};
use clap::{Parser, Subcommand};

mod arceos;
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

const ARCEOS_TARGETS: &[&str] = &[
    "x86_64-unknown-none",
    "riscv64gc-unknown-none-elf",
    "aarch64-unknown-none-softfloat",
    "loongarch64-unknown-none-softfloat",
];

fn supported_targets(os: &str) -> &'static [&'static str] {
    match os {
        "axvisor" => AXVISOR_TARGETS,
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
        } => run_target_test_command("axvisor", &target),
        Commands::Test {
            command: TestCommand::Starry { target },
        } => run_target_test_command("starry", &target),
        Commands::Test {
            command: TestCommand::Arceos { target },
        } => run_arceos_test_command(target.as_deref()).await,
        Commands::Arceos { command } => command.run().await,
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
    let manifest_dir = arceos::config::arceos_manifest_dir()?;

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
        cleanup_arceos_test_configs(&package.crate_dir)?;
        let qemu_config_path =
            ensure_arceos_test_qemu_config_path(&package.crate_dir, &package.name, selected_arch)?;
        let smp = arceos_test_smp_from_qemu_config(&qemu_config_path)?;
        run_arceos_test_package(&manifest_dir, &package.name, arch, smp, &qemu_config_path)
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

fn cleanup_arceos_test_configs(crate_dir: &Path) -> Result<()> {
    for file in [".axconfig.toml", ".arceos.toml"] {
        let path = crate_dir.join(file);
        if path.exists() {
            fs::remove_file(&path)
                .with_context(|| format!("failed to remove {}", path.display()))?;
        }
    }
    Ok(())
}

fn arceos_test_arch(target_arch: Option<Arch>) -> Arch {
    target_arch.unwrap_or_default()
}

fn arceos_test_qemu_config_path(crate_dir: &Path, arch: Arch) -> PathBuf {
    crate_dir.join(format!("qemu-{}.toml", arch.to_qemu_arch()))
}

fn ensure_arceos_test_qemu_config_path(
    crate_dir: &Path,
    package: &str,
    arch: Arch,
) -> Result<PathBuf> {
    let path = arceos_test_qemu_config_path(crate_dir, arch);
    if !path.exists() {
        bail!(
            "missing qemu config for package `{}`: {}",
            package,
            path.display()
        );
    }
    Ok(path)
}

fn arceos_test_smp_from_qemu_config(qemu_config_path: &Path) -> Result<Option<usize>> {
    let contents = fs::read_to_string(qemu_config_path)
        .with_context(|| format!("failed to read {}", qemu_config_path.display()))?;
    let parsed: toml::Value = toml::from_str(&contents)
        .with_context(|| format!("failed to parse {}", qemu_config_path.display()))?;
    let Some(args) = parsed.get("args").and_then(|v| v.as_array()) else {
        return Ok(None);
    };

    for (idx, arg) in args.iter().enumerate() {
        if arg.as_str() == Some("-smp") {
            let value = args.get(idx + 1).with_context(|| {
                format!(
                    "invalid qemu args in {}: `-smp` is missing value",
                    qemu_config_path.display()
                )
            })?;
            let value = value.as_str().with_context(|| {
                format!(
                    "invalid qemu args in {}: `-smp` value must be a string",
                    qemu_config_path.display()
                )
            })?;
            let smp = value.parse::<usize>().with_context(|| {
                format!(
                    "invalid qemu args in {}: `-smp` value `{}` is not a number",
                    qemu_config_path.display(),
                    value
                )
            })?;
            if smp == 0 {
                bail!(
                    "invalid qemu args in {}: `-smp` value must be >= 1",
                    qemu_config_path.display()
                );
            }
            return Ok(Some(smp));
        }
    }

    Ok(None)
}

async fn run_arceos_test_package(
    manifest_dir: &Path,
    package: &str,
    arch: Option<Arch>,
    smp: Option<usize>,
    qemu_config_path: &Path,
) -> Result<()> {
    let target_platform = arch.map(|arch| PlatformResolver::resolve_default_platform_name(&arch));
    let overrides = arceos::config::run_config_override(
        arch.map(|v| v.to_string()),
        package.to_owned(),
        target_platform,
        true,
        None,
        smp,
        false,
        None,
        false,
        None,
        false,
        false,
    )?;
    let ctx = AxContext::new(
        manifest_dir.to_path_buf(),
        overrides,
        Some(package.to_owned()),
        Some(qemu_config_path.to_path_buf()),
    )?;
    arceos::run::run_with_context(ctx).await
}

#[cfg(test)]
fn apply_target_defaults(config: &mut axbuild::arceos::ArceosConfig, arch: Option<Arch>) {
    if let Some(arch) = arch {
        config.arch = arch;
        config.platform = PlatformResolver::resolve_default_platform_name(&arch);
    }
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

    #[test]
    fn supported_targets_returns_arceos_targets() {
        let targets = supported_targets("arceos");
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
    fn parse_arceos_target_maps_to_arch() {
        assert_eq!(
            parse_arceos_target("x86_64-unknown-none").unwrap(),
            Arch::X86_64
        );
        assert_eq!(
            parse_arceos_target("aarch64-unknown-none-softfloat").unwrap(),
            Arch::AArch64
        );
        assert_eq!(
            parse_arceos_target("riscv64gc-unknown-none-elf").unwrap(),
            Arch::RiscV64
        );
        assert_eq!(
            parse_arceos_target("loongarch64-unknown-none-softfloat").unwrap(),
            Arch::LoongArch64
        );
    }

    #[test]
    fn parse_arceos_target_rejects_unknown_target() {
        let err = parse_arceos_target("thumbv7em-none-eabihf").unwrap_err();
        assert!(err.to_string().contains("unsupported target"));
    }

    #[test]
    fn collect_arceos_test_packages_filters_and_sorts() {
        let workspace = PathBuf::from("/ws");
        let packages = vec![
            (
                "z".to_string(),
                PathBuf::from("/ws/test-suit/arceos/task/z/Cargo.toml"),
            ),
            (
                "outside".to_string(),
                PathBuf::from("/ws/components/axbuild/Cargo.toml"),
            ),
            (
                "a".to_string(),
                PathBuf::from("/ws/test-suit/arceos/task/a/Cargo.toml"),
            ),
        ];
        let selected = collect_arceos_test_packages(&workspace, packages);
        assert_eq!(
            selected,
            vec![
                ArceosTestPackage {
                    name: "a".to_string(),
                    crate_dir: PathBuf::from("/ws/test-suit/arceos/task/a"),
                },
                ArceosTestPackage {
                    name: "z".to_string(),
                    crate_dir: PathBuf::from("/ws/test-suit/arceos/task/z"),
                },
            ]
        );
    }

    #[test]
    fn cleanup_arceos_test_configs_removes_target_files_only() {
        let crate_dir = std::env::temp_dir().join(format!(
            "tg-xtask-cleanup-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&crate_dir).unwrap();
        std::fs::write(crate_dir.join(".axconfig.toml"), "x").unwrap();
        std::fs::write(crate_dir.join(".arceos.toml"), "x").unwrap();
        std::fs::write(crate_dir.join("qemu-aarch64.toml"), "x").unwrap();

        cleanup_arceos_test_configs(&crate_dir).unwrap();

        assert!(!crate_dir.join(".axconfig.toml").exists());
        assert!(!crate_dir.join(".arceos.toml").exists());
        assert!(crate_dir.join("qemu-aarch64.toml").exists());

        std::fs::remove_dir_all(&crate_dir).unwrap();
    }

    #[test]
    fn apply_target_defaults_overrides_platform_for_specified_arch() {
        let mut config = axbuild::arceos::ArceosConfig::default();
        apply_target_defaults(&mut config, Some(Arch::RiscV64));
        assert_eq!(config.arch, Arch::RiscV64);
        assert_eq!(config.platform, "riscv64-qemu-virt");
    }

    #[test]
    fn arceos_test_arch_defaults_to_aarch64() {
        assert_eq!(arceos_test_arch(None), Arch::AArch64);
    }

    #[test]
    fn arceos_test_qemu_config_path_uses_target_specific_name() {
        let crate_dir = PathBuf::from("/ws/test-suit/arceos/task/wait_queue");
        assert_eq!(
            arceos_test_qemu_config_path(&crate_dir, Arch::X86_64),
            crate_dir.join("qemu-x86_64.toml")
        );
        assert_eq!(
            arceos_test_qemu_config_path(&crate_dir, Arch::AArch64),
            crate_dir.join("qemu-aarch64.toml")
        );
        assert_eq!(
            arceos_test_qemu_config_path(&crate_dir, Arch::RiscV64),
            crate_dir.join("qemu-riscv64.toml")
        );
        assert_eq!(
            arceos_test_qemu_config_path(&crate_dir, Arch::LoongArch64),
            crate_dir.join("qemu-loongarch64.toml")
        );
    }

    #[test]
    fn ensure_arceos_test_qemu_config_path_fails_when_missing() {
        let crate_dir = std::env::temp_dir().join(format!(
            "tg-xtask-qemu-check-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&crate_dir).unwrap();

        let err =
            ensure_arceos_test_qemu_config_path(&crate_dir, "missing", Arch::AArch64).unwrap_err();
        assert!(err.to_string().contains("missing qemu config"));

        std::fs::remove_dir_all(&crate_dir).unwrap();
    }

    #[test]
    fn arceos_test_smp_from_qemu_config_reads_smp_flag() {
        let crate_dir = std::env::temp_dir().join(format!(
            "tg-xtask-qemu-smp-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&crate_dir).unwrap();
        let qemu_path = crate_dir.join("qemu-aarch64.toml");
        std::fs::write(
            &qemu_path,
            "args = [\"-machine\", \"virt\", \"-smp\", \"4\", \"-nographic\"]\n",
        )
        .unwrap();

        let smp = arceos_test_smp_from_qemu_config(&qemu_path).unwrap();
        assert_eq!(smp, Some(4));

        std::fs::remove_dir_all(&crate_dir).unwrap();
    }
}
