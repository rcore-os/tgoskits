use std::{
    collections::{BTreeMap, HashMap},
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::Command as ProcessCommand,
};

use anyhow::{Context, anyhow, bail};
use cargo_metadata::{DependencyKind, Metadata, Package};
use clap::{Args, Subcommand, ValueEnum};
use ostool::{
    board::RunBoardOptions,
    build::config::{Cargo, CargoBuildProfile},
    ovmf::Arch,
    run::qemu::QemuConfig,
};

use self::plan::{
    DiscoveredKtestPackage, KtestExecutionUnit, KtestRuntime, PlanFailure, QemuPlanSelector,
    build_qemu_plan,
};
use crate::{
    arceos, axvisor,
    context::{AppContext, ResolvedAxvisorRequest, ResolvedBuildRequest, ResolvedStarryRequest},
    starry,
};

mod plan;
#[cfg(test)]
mod tests;

pub(crate) const AXTEST_RUSTFLAGS: &[&str] = &["--cfg", "axtest", "--check-cfg", "cfg(axtest)"];
const AXTEST_FEATURE: &str = "axtest";
const AXTEST_SUITE_OK: &str = "AXTEST_SUITE_OK";
const AXTEST_SUITE_FAIL: &str = "AXTEST_SUITE_FAIL";
const AXTEST_CASE_FAIL: &str = "AXTEST_CASE .* status=fail";
const PANIC_FAIL: &str = "panicked at";
const COVERAGE_IGNORED_SOURCE_REGEX: &str = r"[/\\](\.(cargo|rustup)|target)[/\\]";

#[derive(Args, Debug, Clone)]
pub(crate) struct ArgsKtest {
    #[command(subcommand)]
    pub(crate) command: Command,
}

#[derive(Subcommand, Debug, Clone)]
pub(crate) enum Command {
    /// Run a kernel axtest target in QEMU
    Qemu(ArgsKtestQemu),
    /// Run a kernel axtest target on a remote board
    Board(ArgsKtestBoard),
}

#[derive(Args, Debug, Clone, Default)]
pub(crate) struct ArgsKtestQemu {
    /// Test every workspace package with a direct axtest dev-dependency
    #[arg(long, conflicts_with = "packages")]
    pub(crate) workspace: bool,

    /// Cargo package(s) that own test targets
    #[arg(short = 'p', long = "package", value_name = "PACKAGE")]
    pub(crate) packages: Vec<String>,

    /// Exclude package(s) from workspace selection
    #[arg(long = "exclude", value_name = "PACKAGE")]
    pub(crate) excludes: Vec<String>,

    /// Cargo test target name(s); otherwise run every harness=false test target
    #[arg(long = "test", value_name = "TARGET")]
    pub(crate) tests: Vec<String>,

    /// Target architecture
    #[arg(long, value_name = "ARCH", conflicts_with = "target")]
    pub(crate) arch: Option<String>,

    /// Rust target triple
    #[arg(short = 't', long, value_name = "TRIPLE", conflicts_with = "arch")]
    pub(crate) target: Option<String>,

    /// Features to enable, separated by commas or repeated
    #[arg(long, value_delimiter = ',', value_name = "FEATURES")]
    pub(crate) features: Vec<String>,

    /// Enable every feature of the selected package
    #[arg(long = "all-features", conflicts_with = "no_default_features")]
    pub(crate) all_features: bool,

    /// Do not enable the selected package's default feature
    #[arg(long = "no-default-features")]
    pub(crate) no_default_features: bool,

    /// Cargo build profile
    #[arg(long, value_name = "PROFILE")]
    pub(crate) profile: Option<String>,

    /// Cargo target directory
    #[arg(long = "target-dir", value_name = "DIRECTORY")]
    pub(crate) target_dir: Option<PathBuf>,

    /// Require Cargo.lock to remain unchanged
    #[arg(long)]
    pub(crate) locked: bool,

    /// Run without accessing the network
    #[arg(long)]
    pub(crate) offline: bool,

    /// Equivalent to --locked and --offline
    #[arg(long)]
    pub(crate) frozen: bool,

    /// Build TOML path
    #[arg(long = "config", value_name = "BUILD_TOML")]
    pub(crate) config: Option<PathBuf>,

    /// QEMU TOML path
    #[arg(long = "qemu-config", value_name = "QEMU_TOML")]
    pub(crate) qemu_config: Option<PathBuf>,

    /// Enable axtest coverage capture
    #[arg(long)]
    pub(crate) coverage: bool,

    /// Generate coverage report in the selected format
    #[arg(
        long = "out-fmt",
        value_enum,
        value_name = "FMT",
        requires = "coverage"
    )]
    pub(crate) out_fmt: Option<KtestCoverageOutFmt>,

    /// Continue running execution units after a failure
    #[arg(long = "no-fail-fast")]
    pub(crate) no_fail_fast: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum KtestCoverageOutFmt {
    Html,
}

#[derive(Args, Debug, Clone)]
pub(crate) struct ArgsKtestBoard {
    /// Cargo package that owns the test target
    #[arg(short = 'p', long = "package", value_name = "PACKAGE")]
    pub(crate) package: String,

    /// Cargo test target name
    #[arg(long = "test", value_name = "TARGET")]
    pub(crate) test: String,

    /// Board/default config name
    #[arg(short = 'b', long = "board", value_name = "BOARD")]
    pub(crate) board: String,

    /// Build TOML path
    #[arg(long = "config", value_name = "BUILD_TOML")]
    pub(crate) config: Option<PathBuf>,

    /// Board TOML path
    #[arg(long = "board-config", value_name = "BOARD_TOML")]
    pub(crate) board_config: Option<PathBuf>,

    /// Override ostool board type
    #[arg(long = "board-type", value_name = "TYPE")]
    pub(crate) board_type: Option<String>,

    /// ostool-server host
    #[arg(long)]
    pub(crate) server: Option<String>,

    /// ostool-server port
    #[arg(long)]
    pub(crate) port: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct KtestPackage {
    pub(crate) name: String,
    pub(crate) targets: Vec<KtestTarget>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct KtestTarget {
    pub(crate) name: String,
    pub(crate) kind: KtestTargetKind,
    pub(crate) harness: bool,
    pub(crate) required_features: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KtestTargetKind {
    Lib,
    Test,
    Other,
}

fn is_harness_false_test(target: &KtestTarget) -> bool {
    target.kind == KtestTargetKind::Test && !target.harness
}

pub(crate) async fn run(args: ArgsKtest) -> anyhow::Result<()> {
    match args.command {
        Command::Qemu(args) => run_qemu(args).await,
        Command::Board(args) => run_board(args).await,
    }
}

async fn run_qemu(args: ArgsKtestQemu) -> anyhow::Result<()> {
    let mut app = AppContext::new()?;
    let packages = discover_workspace_ktests(crate::build::cached_workspace_metadata()?)?;
    let plan = build_qemu_plan(&packages, &args.qemu_selector())?;
    validate_unique_config_overrides(&args, &plan)?;
    if plan.is_empty() {
        println!("[axtest] no QEMU execution units selected");
        return Ok(());
    }

    println!("[axtest] execution plan ({} unit(s)):", plan.len());
    for unit in &plan {
        println!(
            "  {} --test {} --target {} runtime={}",
            unit.package,
            unit.test,
            unit.target,
            runtime_name(unit.runtime)
        );
    }

    let package_by_name = packages
        .iter()
        .map(|package| (package.name.as_str(), package))
        .collect::<BTreeMap<_, _>>();
    let mut failures = Vec::new();
    for unit in plan {
        let package = package_by_name
            .get(unit.package.as_str())
            .copied()
            .ok_or_else(|| anyhow!("planned package `{}` disappeared", unit.package))?;
        println!(
            "[axtest] running package={} test={} target={}",
            unit.package, unit.test, unit.target
        );
        if let Err(error) = run_qemu_unit(&mut app, &args, package, &unit).await {
            eprintln!(
                "[axtest] failed package={} test={} target={}: {error:#}",
                unit.package, unit.test, unit.target
            );
            failures.push(PlanFailure { unit, error });
            if !args.no_fail_fast {
                break;
            }
        }
    }
    finish_qemu_plan(failures)
}

async fn run_qemu_unit(
    app: &mut AppContext,
    args: &ArgsKtestQemu,
    package: &DiscoveredKtestPackage,
    unit: &KtestExecutionUnit,
) -> anyhow::Result<()> {
    let target = package
        .targets
        .iter()
        .find(|target| target.name == unit.test)
        .ok_or_else(|| {
            anyhow!(
                "test target `{}` disappeared from package `{}`",
                unit.test,
                unit.package
            )
        })?;
    let package_dir = package
        .manifest_path
        .parent()
        .ok_or_else(|| anyhow!("invalid manifest path {}", package.manifest_path.display()))?;
    let build_config = args.config.clone().unwrap_or_else(|| {
        default_qemu_build_config(
            app.workspace_root(),
            package_dir,
            unit.runtime,
            &unit.arch,
            &unit.target,
        )
    });
    let qemu_config = args.qemu_config.clone().unwrap_or_else(|| {
        default_qemu_run_config(app.workspace_root(), package_dir, unit.runtime, &unit.arch)
    });
    let mut cargo = load_runtime_cargo(
        unit.runtime,
        app.workspace_root(),
        &unit.package,
        &unit.arch,
        &unit.target,
        &build_config,
    )?;
    prepare_ktest_cargo(&mut cargo, target, unit.runtime, args.coverage);
    apply_qemu_cargo_options(&mut cargo, args);
    app.set_debug_mode(false)?;

    let report_session = maybe_start_starry_future_incompat_report(
        unit.runtime,
        app.workspace_root(),
        &unit.arch,
        &cargo,
    )?;
    let build_result = app.build(cargo.clone(), build_config.clone()).await;
    let output = crate::build::finish_future_incompat_report_session(report_session, build_result)?;
    maybe_postprocess_starry_artifact(
        KtestBuildContext {
            runtime: unit.runtime,
            workspace_root: app.workspace_root(),
            package: &unit.package,
            arch: &unit.arch,
            target: &unit.target,
            build_config: &build_config,
        },
        &cargo,
        &output,
    )?;
    let rootfs =
        ensure_runtime_qemu_assets(unit.runtime, app.workspace_root(), &unit.arch, &unit.target)
            .await?;

    let mut qemu = app
        .read_qemu_config_from_path_for_cargo(&cargo, &qemu_config)
        .await
        .with_context(|| format!("failed to read QEMU config {}", qemu_config.display()))?;
    if let Some(rootfs) = rootfs {
        crate::rootfs::qemu::patch_rootfs(
            &mut qemu,
            &rootfs,
            crate::rootfs::qemu::RootfsPatchOptions {
                mode: crate::rootfs::qemu::RootfsPatchMode::EnsureDiskBootNet,
                write_policy: crate::rootfs::qemu::RootfsWritePolicy::Discard,
            },
        )?;
    }
    if unit.runtime == KtestRuntime::Arceos {
        arceos::rootfs::prepare_default_qemu_fat32_rootfs(app.workspace_root(), &qemu)?;
    }
    patch_x86_64_uefi_kernel_loader(&mut qemu, &unit.arch, output.elf_path()).await?;
    apply_ktest_timeout(&mut qemu, unit.runtime, args.coverage);
    apply_axtest_qemu_markers(&mut qemu);
    app.run_qemu_with_axtest_coverage(&cargo, qemu, None)
        .await?;
    if let Some(out_fmt) = args.out_fmt {
        generate_ktest_coverage_report(out_fmt, app.workspace_root(), &cargo, output.elf_path())?;
    }
    Ok(())
}

impl ArgsKtestQemu {
    fn qemu_selector(&self) -> QemuPlanSelector {
        QemuPlanSelector {
            workspace: self.workspace,
            packages: self.packages.clone(),
            excludes: self.excludes.clone(),
            tests: self.tests.clone(),
            arch: self.arch.clone(),
            target: self.target.clone(),
        }
    }
}

fn validate_unique_config_overrides(
    args: &ArgsKtestQemu,
    plan: &[KtestExecutionUnit],
) -> anyhow::Result<()> {
    if plan.len() > 1 && (args.config.is_some() || args.qemu_config.is_some()) {
        bail!("--config and --qemu-config require exactly one QEMU execution unit");
    }
    Ok(())
}

fn finish_qemu_plan(failures: Vec<PlanFailure>) -> anyhow::Result<()> {
    if failures.is_empty() {
        println!("[axtest] all QEMU execution units passed");
        return Ok(());
    }
    eprintln!("[axtest] {} execution unit(s) failed:", failures.len());
    for failure in &failures {
        eprintln!(
            "  {} --test {} --target {}: {:#}",
            failure.unit.package, failure.unit.test, failure.unit.target, failure.error
        );
    }
    bail!("{} axtest QEMU execution unit(s) failed", failures.len())
}

async fn patch_x86_64_uefi_kernel_loader(
    qemu: &mut QemuConfig,
    arch: &str,
    elf_path: &Path,
) -> anyhow::Result<()> {
    if arch != "x86_64" || !qemu.uefi {
        return Ok(());
    }

    let firmware = crate::support::ovmf::OvmfFirmware::fetch(Arch::X64).await?;
    let vars = elf_path.with_extension("vars.fd");
    fs::copy(firmware.vars(), &vars).with_context(|| {
        format!(
            "failed to copy OVMF vars from {} to {}",
            firmware.vars().display(),
            vars.display()
        )
    })?;
    apply_x86_64_uefi_kernel_loader(qemu, firmware.code(), &vars);
    Ok(())
}

fn apply_x86_64_uefi_kernel_loader(qemu: &mut QemuConfig, code: &Path, vars: &Path) {
    // Keep ostool's BIN conversion and QEMU kernel loader while supplying
    // the shared OVMF cache as explicit pflash drives.
    qemu.uefi = false;
    qemu.to_bin = true;
    qemu.args.extend([
        "-drive".to_string(),
        format!(
            "if=pflash,format=raw,unit=0,readonly=on,file={}",
            code.display()
        ),
        "-drive".to_string(),
        format!("if=pflash,format=raw,unit=1,file={}", vars.display()),
    ]);
}

#[derive(Debug, Clone, Copy)]
struct KtestBuildContext<'a> {
    runtime: KtestRuntime,
    workspace_root: &'a Path,
    package: &'a str,
    arch: &'a str,
    target: &'a str,
    build_config: &'a Path,
}

async fn ensure_runtime_qemu_assets(
    runtime: KtestRuntime,
    workspace_root: &Path,
    arch: &str,
    target: &str,
) -> anyhow::Result<Option<PathBuf>> {
    if runtime == KtestRuntime::Starry {
        let rootfs = starry::rootfs::ensure_rootfs_in_tmp_dir(workspace_root, arch, target).await?;
        return Ok(Some(rootfs));
    }
    if runtime == KtestRuntime::Axvisor {
        let rootfs = crate::image::storage::ensure_rootfs_for_arch(workspace_root, arch).await?;
        return Ok(Some(rootfs));
    }
    Ok(None)
}

async fn run_board(args: ArgsKtestBoard) -> anyhow::Result<()> {
    let mut app = AppContext::new()?;
    let discovered = load_discovered_ktest_package(&args.package)?;
    if !discovered.uses_workspace_axtest {
        bail!(
            "package `{}` must declare workspace `axtest` directly in [dev-dependencies]",
            args.package
        );
    }
    let package = KtestPackage {
        name: discovered.name.clone(),
        targets: discovered.targets.clone(),
    };
    let target = select_ktest_target(&package, Some(&args.test))?;
    let runtime = discovered.runtime;
    let package_dir = discovered.manifest_path.parent().ok_or_else(|| {
        anyhow!(
            "invalid manifest path {}",
            discovered.manifest_path.display()
        )
    })?;
    let board = args.board;
    let build_config = args.config.unwrap_or_else(|| {
        default_board_build_config(app.workspace_root(), package_dir, runtime, &board)
    });
    let board_config_path = args.board_config.unwrap_or_else(|| {
        default_board_run_config(app.workspace_root(), package_dir, runtime, &board)
    });
    let target_from_config =
        load_target_from_build_config(runtime, &build_config).with_context(|| {
            format!(
                "failed to read build target from {}",
                build_config.display()
            )
        })?;
    let triple = target_from_config
        .ok_or_else(|| anyhow!("board ktest requires target in {}", build_config.display()))?;
    let arch = crate::context::arch_for_target_checked(&triple)?;
    let mut cargo = load_runtime_cargo(
        runtime,
        app.workspace_root(),
        &args.package,
        arch,
        &triple,
        &build_config,
    )?;
    prepare_ktest_cargo(&mut cargo, target, runtime, false);

    let board_config = app
        .read_board_run_config_from_path_for_cargo(&cargo, &board_config_path)
        .await
        .with_context(|| {
            format!(
                "failed to read board config {}",
                board_config_path.display()
            )
        })?;
    let report_session =
        maybe_start_starry_future_incompat_report(runtime, app.workspace_root(), arch, &cargo)?;
    let build_result = app.build(cargo.clone(), build_config.clone()).await;
    let output = crate::build::finish_future_incompat_report_session(report_session, build_result)?;
    maybe_postprocess_starry_artifact(
        KtestBuildContext {
            runtime,
            workspace_root: app.workspace_root(),
            package: &args.package,
            arch,
            target: &triple,
            build_config: &build_config,
        },
        &cargo,
        &output,
    )?;
    app.board_prepared_elf(
        output.elf_path().to_path_buf(),
        cargo.to_bin,
        build_config,
        board_config,
        RunBoardOptions {
            board_type: args.board_type,
            server: args.server,
            port: args.port,
        },
    )
    .await
}

fn load_discovered_ktest_package(package: &str) -> anyhow::Result<DiscoveredKtestPackage> {
    discover_workspace_ktests(crate::build::cached_workspace_metadata()?)?
        .into_iter()
        .find(|candidate| candidate.name == package)
        .ok_or_else(|| anyhow!("workspace package `{package}` not found"))
}

fn discover_workspace_ktests(metadata: &Metadata) -> anyhow::Result<Vec<DiscoveredKtestPackage>> {
    let workspace_members = metadata
        .workspace_members
        .iter()
        .collect::<std::collections::BTreeSet<_>>();
    let axtest = metadata
        .packages
        .iter()
        .find(|package| {
            package.name.as_str() == AXTEST_FEATURE && workspace_members.contains(&package.id)
        })
        .ok_or_else(|| anyhow!("workspace package `axtest` not found"))?;
    let axtest_dir = axtest
        .manifest_path
        .parent()
        .ok_or_else(|| anyhow!("invalid axtest manifest path {}", axtest.manifest_path))?;
    let mut packages = Vec::new();

    for package in metadata
        .packages
        .iter()
        .filter(|package| workspace_members.contains(&package.id))
    {
        let uses_workspace_axtest = package.dependencies.iter().any(|dependency| {
            dependency.kind == DependencyKind::Development
                && dependency.name == AXTEST_FEATURE
                && dependency.path.as_deref() == Some(axtest_dir)
        });
        let ktest_package = ktest_package_from_metadata(package)?;
        packages.push(DiscoveredKtestPackage {
            name: ktest_package.name,
            manifest_path: package.manifest_path.as_std_path().to_path_buf(),
            uses_workspace_axtest,
            runtime: runtime_from_package_metadata(package)?,
            targets: ktest_package.targets,
            docs_rs_targets: docs_rs_targets(package)?,
        });
    }
    packages.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(packages)
}

fn runtime_from_package_metadata(package: &Package) -> anyhow::Result<KtestRuntime> {
    let Some(runtime) = package
        .metadata
        .get("axtest")
        .and_then(|axtest| axtest.get("runtime"))
        .and_then(serde_json::Value::as_str)
    else {
        return Ok(KtestRuntime::Arceos);
    };
    match runtime {
        "arceos" => Ok(KtestRuntime::Arceos),
        "starry" => Ok(KtestRuntime::Starry),
        "axvisor" => Ok(KtestRuntime::Axvisor),
        "board" => Ok(KtestRuntime::Board),
        _ => bail!(
            "package `{}` has unsupported [package.metadata.axtest] runtime `{runtime}`",
            package.name
        ),
    }
}

fn docs_rs_targets(package: &Package) -> anyhow::Result<Option<Vec<String>>> {
    let Some(targets) = package
        .metadata
        .get("docs")
        .and_then(|docs| docs.get("rs"))
        .and_then(|docs_rs| docs_rs.get("targets"))
    else {
        return Ok(None);
    };
    let targets = targets.as_array().ok_or_else(|| {
        anyhow!(
            "package `{}` must declare [package.metadata.docs.rs].targets as an array",
            package.name
        )
    })?;
    targets
        .iter()
        .map(|target| {
            target.as_str().map(str::to_string).ok_or_else(|| {
                anyhow!("package `{}` has a non-string docs.rs target", package.name)
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()
        .map(Some)
}

fn ktest_package_from_metadata(package: &Package) -> anyhow::Result<KtestPackage> {
    let harness = test_harness_flags(package.manifest_path.as_std_path())?;
    let targets = package
        .targets
        .iter()
        .map(|target| {
            let kind = if target.is_test() {
                KtestTargetKind::Test
            } else if target.is_lib() {
                KtestTargetKind::Lib
            } else {
                KtestTargetKind::Other
            };
            KtestTarget {
                name: target.name.clone(),
                kind,
                harness: harness.get(&target.name).copied().unwrap_or(true),
                required_features: target.required_features.clone(),
            }
        })
        .collect();
    Ok(KtestPackage {
        name: package.name.to_string(),
        targets,
    })
}

fn test_harness_flags(manifest_path: &Path) -> anyhow::Result<HashMap<String, bool>> {
    let content = fs::read_to_string(manifest_path)
        .with_context(|| format!("failed to read {}", manifest_path.display()))?;
    let value: toml::Value = toml::from_str(&content)
        .with_context(|| format!("failed to parse {}", manifest_path.display()))?;
    let mut flags = HashMap::new();
    if let Some(tests) = value.get("test").and_then(toml::Value::as_array) {
        for entry in tests {
            let Some(table) = entry.as_table() else {
                continue;
            };
            let Some(name) = table.get("name").and_then(toml::Value::as_str) else {
                continue;
            };
            let harness = table
                .get("harness")
                .and_then(toml::Value::as_bool)
                .unwrap_or(true);
            flags.insert(name.to_string(), harness);
        }
    }
    Ok(flags)
}

pub(crate) fn select_ktest_target<'a>(
    package: &'a KtestPackage,
    explicit: Option<&str>,
) -> anyhow::Result<&'a KtestTarget> {
    if let Some(name) = explicit {
        let target = package
            .targets
            .iter()
            .find(|target| target.name == name)
            .ok_or_else(|| {
                anyhow!(
                    "test target `{name}` not found in package `{}`",
                    package.name
                )
            })?;
        if target.kind != KtestTargetKind::Test || target.harness {
            bail!(
                "test target `{}` in package `{}` must be a harness=false [[test]] target",
                target.name,
                package.name
            );
        }
        return Ok(target);
    }

    let candidates = package
        .targets
        .iter()
        .filter(|target| is_harness_false_test(target))
        .collect::<Vec<_>>();
    match candidates.as_slice() {
        [target] => Ok(*target),
        [] => bail!(
            "package `{}` has no harness=false [[test]] target; pass --test after adding one",
            package.name
        ),
        many => {
            let names = many
                .iter()
                .map(|target| target.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            bail!(
                "package `{}` has multiple harness=false [[test]] targets ({names}); pass --test",
                package.name
            )
        }
    }
}

fn prepare_ktest_cargo(
    cargo: &mut Cargo,
    target: &KtestTarget,
    runtime: KtestRuntime,
    coverage: bool,
) {
    cargo.bin = None;
    cargo.test = Some(target.name.clone());
    remove_cargo_target_selector_args(&mut cargo.args);
    cargo.env.remove("AXBUILD_STARRY_BIN");
    // `ktest` is an explicit command: the harness feature and the target's
    // declared required features are the only additions made here.
    ensure_feature(cargo, AXTEST_FEATURE);
    for feature in &target.required_features {
        ensure_feature(cargo, feature);
    }
    if matches!(runtime, KtestRuntime::Arceos | KtestRuntime::Board) {
        ensure_feature(cargo, "ax-std/arceos");
    }
    crate::build::append_cargo_rustflags(cargo, AXTEST_RUSTFLAGS);
    if coverage {
        cargo
            .env
            .insert("AXTEST_COVERAGE".to_string(), "y".to_string());
        crate::support::axtest_coverage::prepare_cargo(cargo);
    } else {
        // Coverage is owned by the explicit ktest CLI flag. Package-local
        // build configuration must not turn it on implicitly.
        cargo.env.remove("AXTEST_COVERAGE");
    }
}

fn generate_ktest_coverage_report(
    out_fmt: KtestCoverageOutFmt,
    workspace_root: &Path,
    cargo: &Cargo,
    elf_path: &Path,
) -> anyhow::Result<()> {
    match out_fmt {
        KtestCoverageOutFmt::Html => generate_ktest_coverage_html(workspace_root, cargo, elf_path),
    }
}

fn generate_ktest_coverage_html(
    workspace_root: &Path,
    cargo: &Cargo,
    elf_path: &Path,
) -> anyhow::Result<()> {
    let paths = crate::support::axtest_coverage::AxtestCoveragePaths::new(
        workspace_root,
        &cargo.package,
        cargo
            .test
            .as_deref()
            .context("axtest coverage report requires a Cargo test target")?,
        &cargo.target,
    )?;
    let profraw_path = paths.profraw_path;
    if !profraw_path.is_file() {
        bail!(
            "coverage profile was not found at {}; run ktest qemu with --coverage first",
            profraw_path.display()
        );
    }
    if !elf_path.is_file() {
        bail!("coverage binary was not found at {}", elf_path.display());
    }

    let profdata_path = profraw_path.with_extension("profdata");
    let stem = profraw_path
        .file_stem()
        .ok_or_else(|| anyhow!("invalid coverage profile path {}", profraw_path.display()))?
        .to_string_lossy();
    let html_dir = profraw_path.with_file_name(format!("{stem}-html"));
    if html_dir.exists() {
        fs::remove_dir_all(&html_dir)
            .with_context(|| format!("failed to remove {}", html_dir.display()))?;
    }

    let llvm_profdata = find_llvm_tool("llvm-profdata");
    let llvm_cov = find_llvm_tool("llvm-cov");
    run_tool(
        &llvm_profdata,
        [
            OsString::from("merge"),
            OsString::from("-sparse"),
            profraw_path.as_os_str().to_os_string(),
            OsString::from("-o"),
            profdata_path.as_os_str().to_os_string(),
        ],
    )
    .with_context(|| format!("failed to create {}", profdata_path.display()))?;
    run_tool(
        &llvm_cov,
        llvm_cov_html_args(elf_path, &profdata_path, &html_dir),
    )
    .with_context(|| {
        format!(
            "failed to create HTML coverage report in {}",
            html_dir.display()
        )
    })?;

    println!("  coverage profdata: {}", profdata_path.display());
    println!("  coverage html: {}/index.html", html_dir.display());
    Ok(())
}

fn llvm_cov_html_args(elf_path: &Path, profdata_path: &Path, html_dir: &Path) -> Vec<OsString> {
    vec![
        OsString::from("show"),
        elf_path.as_os_str().to_os_string(),
        OsString::from(format!("-instr-profile={}", profdata_path.display())),
        OsString::from("-format=html"),
        OsString::from(format!("-output-dir={}", html_dir.display())),
        OsString::from(format!(
            "-ignore-filename-regex={COVERAGE_IGNORED_SOURCE_REGEX}"
        )),
    ]
}

fn find_llvm_tool(tool: &str) -> PathBuf {
    if let Ok(output) = ProcessCommand::new("rustc")
        .args(["--print", "sysroot"])
        .output()
        && output.status.success()
    {
        let sysroot = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let rustlib = Path::new(&sysroot).join("lib/rustlib");
        if let Ok(entries) = fs::read_dir(rustlib) {
            for entry in entries.flatten() {
                let candidate = entry.path().join("bin").join(tool);
                if candidate.is_file() {
                    return candidate;
                }
            }
        }
    }
    PathBuf::from(tool)
}

fn run_tool<I>(tool: &Path, args: I) -> anyhow::Result<()>
where
    I: IntoIterator<Item = OsString>,
{
    let status = ProcessCommand::new(tool)
        .args(args)
        .status()
        .with_context(|| format!("failed to run {}", tool.display()))?;
    if !status.success() {
        bail!("{} failed with status {status}", tool.display());
    }
    Ok(())
}

fn ensure_feature(cargo: &mut Cargo, feature: &str) {
    if !cargo.features.iter().any(|candidate| candidate == feature) {
        cargo.features.push(feature.to_string());
    }
}

fn apply_qemu_cargo_options(cargo: &mut Cargo, args: &ArgsKtestQemu) {
    for feature in &args.features {
        ensure_feature(cargo, feature);
    }
    if args.all_features {
        ensure_cargo_arg(&mut cargo.args, "--all-features");
    }
    if args.no_default_features {
        ensure_cargo_arg(&mut cargo.args, "--no-default-features");
    }
    if args.locked {
        ensure_cargo_arg(&mut cargo.args, "--locked");
    }
    if args.offline {
        ensure_cargo_arg(&mut cargo.args, "--offline");
    }
    if args.frozen {
        ensure_cargo_arg(&mut cargo.args, "--frozen");
    }
    if let Some(target_dir) = &args.target_dir {
        remove_cargo_value_arg(&mut cargo.args, "--target-dir");
        cargo
            .args
            .extend(["--target-dir".to_string(), target_dir.display().to_string()]);
    }
    if let Some(profile) = &args.profile {
        remove_cargo_value_arg(&mut cargo.args, "--profile");
        cargo.profile = Some(if profile == "release" {
            CargoBuildProfile::Release
        } else {
            CargoBuildProfile::Debug
        });
        if !matches!(profile.as_str(), "dev" | "debug" | "release") {
            cargo
                .args
                .extend(["--profile".to_string(), profile.clone()]);
        }
    }
}

fn ensure_cargo_arg(args: &mut Vec<String>, arg: &str) {
    if !args.iter().any(|candidate| candidate == arg) {
        args.push(arg.to_string());
    }
}

fn remove_cargo_value_arg(args: &mut Vec<String>, name: &str) {
    let equals_prefix = format!("{name}=");
    let mut filtered = Vec::with_capacity(args.len());
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == name {
            let _ = iter.next();
            continue;
        }
        if arg.starts_with(&equals_prefix) {
            continue;
        }
        filtered.push(arg.clone());
    }
    *args = filtered;
}

fn remove_cargo_target_selector_args(args: &mut Vec<String>) {
    let mut filtered = Vec::with_capacity(args.len());
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if matches!(arg.as_str(), "--bin" | "--test") {
            let _ = iter.next();
            continue;
        }
        if arg.starts_with("--bin=") || arg.starts_with("--test=") {
            continue;
        }
        filtered.push(arg.clone());
    }
    *args = filtered;
}

fn apply_axtest_qemu_markers(qemu: &mut QemuConfig) {
    ensure_regex(&mut qemu.success_regex, AXTEST_SUITE_OK);
    ensure_regex(&mut qemu.fail_regex, PANIC_FAIL);
    ensure_regex(&mut qemu.fail_regex, AXTEST_SUITE_FAIL);
    ensure_regex(&mut qemu.fail_regex, AXTEST_CASE_FAIL);
}

fn apply_ktest_timeout(qemu: &mut QemuConfig, runtime: KtestRuntime, coverage: bool) {
    if qemu.timeout.is_some() {
        return;
    }
    qemu.timeout = Some(match (runtime, coverage) {
        (KtestRuntime::Arceos, false) => 60,
        (KtestRuntime::Arceos, true) => 120,
        (KtestRuntime::Starry | KtestRuntime::Axvisor, false) => 300,
        (KtestRuntime::Starry | KtestRuntime::Axvisor, true) => 360,
        (KtestRuntime::Board, _) => return,
    });
}

fn ensure_regex(regexes: &mut Vec<String>, regex: &str) {
    if !regexes.iter().any(|candidate| candidate == regex) {
        regexes.push(regex.to_string());
    }
}

fn runtime_name(runtime: KtestRuntime) -> &'static str {
    match runtime {
        KtestRuntime::Arceos => "arceos",
        KtestRuntime::Starry => "starry",
        KtestRuntime::Axvisor => "axvisor",
        KtestRuntime::Board => "board",
    }
}

fn load_runtime_cargo(
    runtime: KtestRuntime,
    workspace_root: &Path,
    package: &str,
    arch: &str,
    target: &str,
    build_config: &Path,
) -> anyhow::Result<Cargo> {
    match runtime {
        KtestRuntime::Arceos | KtestRuntime::Board => {
            let request = arceos_request(package, arch, target, build_config);
            arceos::build::load_cargo_config(&request)
        }
        KtestRuntime::Starry => {
            let request = starry_request(package, arch, target, build_config);
            starry::build::load_cargo_config(&request)
        }
        KtestRuntime::Axvisor => {
            let request = axvisor_request(workspace_root, package, arch, target, build_config);
            axvisor::build::load_cargo_config(&request)
        }
    }
}

fn load_target_from_build_config(
    runtime: KtestRuntime,
    build_config: &Path,
) -> anyhow::Result<Option<String>> {
    match runtime {
        KtestRuntime::Arceos | KtestRuntime::Board => load_target_from_toml(build_config),
        KtestRuntime::Starry => starry::build::load_target_from_build_config(build_config),
        KtestRuntime::Axvisor => axvisor::build::load_target_from_build_config(build_config),
    }
}

fn load_target_from_toml(path: &Path) -> anyhow::Result<Option<String>> {
    let contents = fs::read_to_string(path)
        .with_context(|| format!("failed to read build config {}", path.display()))?;
    let config: toml::Value = toml::from_str(&contents)
        .with_context(|| format!("failed to parse build config {}", path.display()))?;
    Ok(config
        .get("target")
        .and_then(toml::Value::as_str)
        .map(str::to_string))
}

fn maybe_postprocess_starry_artifact(
    ctx: KtestBuildContext<'_>,
    cargo: &Cargo,
    output: &ostool::build::CargoBuildOutput,
) -> anyhow::Result<()> {
    if ctx.runtime != KtestRuntime::Starry {
        return Ok(());
    }
    let request = starry_request(ctx.package, ctx.arch, ctx.target, ctx.build_config);
    starry::build::postprocess_starry_artifact(ctx.workspace_root, &request, cargo, output)
}

fn maybe_start_starry_future_incompat_report(
    runtime: KtestRuntime,
    workspace_root: &Path,
    arch: &str,
    cargo: &Cargo,
) -> anyhow::Result<Option<crate::build::FutureIncompatReportSession>> {
    if runtime != KtestRuntime::Starry || arch != "aarch64" {
        return Ok(None);
    }
    let target_dir = crate::build::cargo_target_dir_for(workspace_root, &cargo.args)?;
    crate::build::start_future_incompat_report_session(&target_dir).map(Some)
}

fn starry_request(
    package: &str,
    arch: &str,
    target: &str,
    build_config: &Path,
) -> ResolvedStarryRequest {
    ResolvedStarryRequest {
        package: package.to_string(),
        arch: arch.to_string(),
        target: target.to_string(),
        smp: None,
        debug: false,
        build_info_path: build_config.to_path_buf(),
        build_info_override: None,
        qemu_config: None,
        uboot_config: None,
    }
}

fn arceos_request(
    package: &str,
    arch: &str,
    target: &str,
    build_config: &Path,
) -> ResolvedBuildRequest {
    ResolvedBuildRequest {
        package: package.to_string(),
        arch: arch.to_string(),
        target: target.to_string(),
        smp: None,
        debug: false,
        build_info_path: build_config.to_path_buf(),
        qemu_config: None,
        uboot_config: None,
    }
}

fn axvisor_request(
    workspace_root: &Path,
    package: &str,
    arch: &str,
    target: &str,
    build_config: &Path,
) -> ResolvedAxvisorRequest {
    ResolvedAxvisorRequest {
        package: package.to_string(),
        axvisor_dir: workspace_root.join("os/axvisor"),
        arch: arch.to_string(),
        target: target.to_string(),
        smp: None,
        debug: false,
        build_info_path: build_config.to_path_buf(),
        qemu_config: None,
        uboot_config: None,
        vmconfigs: Vec::new(),
    }
}

fn default_qemu_build_config(
    workspace_root: &Path,
    package_dir: &Path,
    runtime: KtestRuntime,
    arch: &str,
    target: &str,
) -> PathBuf {
    let package_config = package_dir.join(format!("build-{target}.toml"));
    if package_config.exists() {
        package_config
    } else {
        runtime_config_root(workspace_root, runtime).join(format!("configs/board/qemu-{arch}.toml"))
    }
}

fn default_qemu_run_config(
    workspace_root: &Path,
    package_dir: &Path,
    runtime: KtestRuntime,
    arch: &str,
) -> PathBuf {
    let package_config = package_dir.join(format!("qemu-{arch}.toml"));
    if package_config.exists() {
        package_config
    } else {
        runtime_config_root(workspace_root, runtime).join(format!("configs/qemu/qemu-{arch}.toml"))
    }
}

fn default_board_build_config(
    workspace_root: &Path,
    package_dir: &Path,
    runtime: KtestRuntime,
    board: &str,
) -> PathBuf {
    let package_config = package_dir.join(format!("build-{board}.toml"));
    if package_config.exists() {
        return package_config;
    }
    let mut package_configs = fs::read_dir(package_dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("build-") && name.ends_with(".toml"))
        })
        .collect::<Vec<_>>();
    package_configs.sort();
    if let [only] = package_configs.as_slice() {
        return only.clone();
    }
    runtime_config_root(workspace_root, runtime).join(format!("configs/board/{board}.toml"))
}

fn default_board_run_config(
    workspace_root: &Path,
    package_dir: &Path,
    runtime: KtestRuntime,
    board: &str,
) -> PathBuf {
    let package_config = package_dir.join(format!("board-{board}.toml"));
    if package_config.exists() {
        return package_config;
    }
    let root = runtime_config_root(workspace_root, runtime);
    let starry_board_config = root.join(format!("configs/board/{board}-board.toml"));
    if runtime != KtestRuntime::Axvisor && starry_board_config.exists() {
        starry_board_config
    } else {
        root.join(format!("configs/board/{board}.toml"))
    }
}

fn runtime_config_root(workspace_root: &Path, runtime: KtestRuntime) -> PathBuf {
    match runtime {
        KtestRuntime::Arceos | KtestRuntime::Board => workspace_root.join("os/arceos"),
        KtestRuntime::Starry => workspace_root.join("os/StarryOS"),
        KtestRuntime::Axvisor => workspace_root.join("os/axvisor"),
    }
}
