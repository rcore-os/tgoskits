use std::{
    collections::HashMap,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::Command as ProcessCommand,
};

use anyhow::{Context, anyhow, bail};
use cargo_metadata::Package;
use clap::{Args, Subcommand, ValueEnum};
use ostool::{board::RunBoardOptions, build::config::Cargo, run::qemu::QemuConfig};

use crate::{
    axvisor,
    context::{
        AppContext, ResolvedAxvisorRequest, ResolvedStarryRequest, resolve_axvisor_arch_and_target,
        resolve_starry_arch_and_target,
    },
    starry,
};

#[cfg(test)]
mod tests;

const AXTEST_RUSTFLAGS: &[&str] = &["--cfg", "axtest", "--check-cfg", "cfg(axtest)"];
const AXTEST_FEATURE: &str = "axtest";
const AXTEST_SUITE_OK: &str = "AXTEST_SUITE_OK";
const AXTEST_SUITE_FAIL: &str = "AXTEST_SUITE_FAIL";
const AXTEST_CASE_FAIL: &str = "AXTEST_CASE .* status=fail";
const PANIC_FAIL: &str = "panicked at";

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

#[derive(Args, Debug, Clone)]
pub(crate) struct ArgsKtestQemu {
    /// Cargo package that owns the test target
    #[arg(short = 'p', long = "package", value_name = "PACKAGE")]
    pub(crate) package: String,

    /// Cargo test target name. Omit only when the package has exactly one harness=false test target
    #[arg(long = "test", value_name = "TARGET")]
    pub(crate) test: Option<String>,

    /// Target architecture
    #[arg(long, value_name = "ARCH", conflicts_with = "target")]
    pub(crate) arch: Option<String>,

    /// Rust target triple
    #[arg(short = 't', long, value_name = "TRIPLE", conflicts_with = "arch")]
    pub(crate) target: Option<String>,

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KtestRuntime {
    Starry,
    Axvisor,
}

pub(crate) async fn run(args: ArgsKtest) -> anyhow::Result<()> {
    match args.command {
        Command::Qemu(args) => run_qemu(args).await,
        Command::Board(args) => run_board(args).await,
    }
}

async fn run_qemu(args: ArgsKtestQemu) -> anyhow::Result<()> {
    let mut app = AppContext::new()?;
    let package = load_ktest_package(&args.package)?;
    let target = select_ktest_target(&package, args.test.as_deref())?;
    let runtime = runtime_for_package(app.workspace_root(), &args.package)?;
    let (arch, triple) = resolve_arch_and_target(runtime, args.arch, args.target)?;
    let build_config = args
        .config
        .unwrap_or_else(|| default_qemu_build_config(app.workspace_root(), &args.package, &arch));
    let qemu_config = args
        .qemu_config
        .unwrap_or_else(|| default_qemu_run_config(app.workspace_root(), &args.package, &arch));
    let mut cargo = load_runtime_cargo(
        runtime,
        app.workspace_root(),
        &args.package,
        &arch,
        &triple,
        &build_config,
    )?;
    prepare_ktest_cargo(&mut cargo, target, args.coverage);
    app.set_debug_mode(false)?;

    let output = app.build(cargo.clone(), build_config.clone()).await?;
    maybe_postprocess_starry_artifact(
        KtestBuildContext {
            runtime,
            workspace_root: app.workspace_root(),
            package: &args.package,
            arch: &arch,
            target: &triple,
            build_config: &build_config,
        },
        &cargo,
        &output,
    )?;
    let rootfs = ensure_runtime_qemu_assets(runtime, app.workspace_root(), &arch, &triple).await?;

    let mut qemu = app
        .read_qemu_config_from_path_for_cargo(&cargo, &qemu_config)
        .await
        .with_context(|| format!("failed to read QEMU config {}", qemu_config.display()))?;
    if let Some(rootfs) = rootfs {
        crate::rootfs::qemu::patch_rootfs(
            &mut qemu,
            &rootfs,
            crate::rootfs::qemu::RootfsPatchMode::EnsureDiskBootNet,
        );
    }
    patch_system_x86_64_uefi_kernel_loader(&mut qemu, &arch, output.elf_path())?;
    apply_axtest_qemu_markers(&mut qemu);
    app.run_qemu_with_axtest_coverage(&cargo, qemu, None)
        .await?;
    if let Some(out_fmt) = args.out_fmt {
        generate_ktest_coverage_report(out_fmt, app.workspace_root(), &cargo, output.elf_path())?;
    }
    Ok(())
}

fn patch_system_x86_64_uefi_kernel_loader(
    qemu: &mut QemuConfig,
    arch: &str,
    elf_path: &Path,
) -> anyhow::Result<()> {
    if arch != "x86_64" || !qemu.uefi {
        return Ok(());
    }

    let Some((code, vars_template)) = find_system_x86_64_ovmf_pair() else {
        return Ok(());
    };

    let vars = elf_path.with_extension("vars.fd");
    fs::copy(vars_template, &vars).with_context(|| {
        format!(
            "failed to copy OVMF vars from {} to {}",
            vars_template.display(),
            vars.display()
        )
    })?;
    apply_system_x86_64_uefi_kernel_loader(qemu, code, &vars);
    Ok(())
}

fn apply_system_x86_64_uefi_kernel_loader(qemu: &mut QemuConfig, code: &Path, vars: &Path) {
    // Keep ostool's BIN conversion and QEMU `-kernel` loader, but bypass its
    // prebuilt OVMF path. QEMU can load this x86_64 UEFI image directly via
    // `-kernel` when system OVMF pflash drives are supplied explicitly.
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

fn find_system_x86_64_ovmf_pair() -> Option<(&'static Path, &'static Path)> {
    x86_64_system_ovmf_candidates()
        .iter()
        .copied()
        .map(|(code, vars)| (Path::new(code), Path::new(vars)))
        .find(|(code, vars)| code.is_file() && vars.is_file())
}

fn x86_64_system_ovmf_candidates() -> &'static [(&'static str, &'static str)] {
    &[
        (
            "/usr/share/OVMF/OVMF_CODE.fd",
            "/usr/share/OVMF/OVMF_VARS.fd",
        ),
        (
            "/usr/share/OVMF/OVMF_CODE_4M.fd",
            "/usr/share/OVMF/OVMF_VARS_4M.fd",
        ),
        ("/usr/share/ovmf/OVMF.fd", "/usr/share/OVMF/OVMF_VARS.fd"),
        ("/usr/share/qemu/OVMF.fd", "/usr/share/OVMF/OVMF_VARS.fd"),
    ]
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
    let package = load_ktest_package(&args.package)?;
    let target = select_ktest_target(&package, Some(&args.test))?;
    let runtime = runtime_for_package(app.workspace_root(), &args.package)?;
    let board = args.board;
    let build_config = args
        .config
        .unwrap_or_else(|| default_board_build_config(app.workspace_root(), &args.package, &board));
    let board_config_path = args
        .board_config
        .unwrap_or_else(|| default_board_run_config(app.workspace_root(), &args.package, &board));
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
    prepare_ktest_cargo(&mut cargo, target, false);

    let board_config = app
        .read_board_run_config_from_path_for_cargo(&cargo, &board_config_path)
        .await
        .with_context(|| {
            format!(
                "failed to read board config {}",
                board_config_path.display()
            )
        })?;
    let output = app.build(cargo.clone(), build_config.clone()).await?;
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

fn load_ktest_package(package: &str) -> anyhow::Result<KtestPackage> {
    let metadata = crate::build::cached_workspace_metadata()?;
    let package = metadata
        .packages
        .iter()
        .find(|candidate| candidate.name.as_str() == package)
        .ok_or_else(|| anyhow!("workspace package `{package}` not found"))?;
    ktest_package_from_metadata(package)
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
        .filter(|target| target.kind == KtestTargetKind::Test && !target.harness)
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

fn prepare_ktest_cargo(cargo: &mut Cargo, target: &KtestTarget, coverage: bool) {
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
    crate::build::append_encoded_rustflags(cargo, AXTEST_RUSTFLAGS);
    if coverage {
        cargo
            .env
            .insert("AXTEST_COVERAGE".to_string(), "y".to_string());
        crate::support::axtest_coverage::prepare_cargo(cargo);
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
        [
            OsString::from("show"),
            elf_path.as_os_str().to_os_string(),
            OsString::from(format!("-instr-profile={}", profdata_path.display())),
            OsString::from("-format=html"),
            OsString::from(format!("-output-dir={}", html_dir.display())),
        ],
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

fn ensure_regex(regexes: &mut Vec<String>, regex: &str) {
    if !regexes.iter().any(|candidate| candidate == regex) {
        regexes.push(regex.to_string());
    }
}

fn runtime_for_package(workspace_root: &Path, package: &str) -> anyhow::Result<KtestRuntime> {
    if package == axvisor::build::AXVISOR_PACKAGE {
        return Ok(KtestRuntime::Axvisor);
    }
    let metadata = crate::build::cached_workspace_metadata()?;
    let package = metadata
        .packages
        .iter()
        .find(|candidate| candidate.name.as_str() == package)
        .ok_or_else(|| anyhow!("workspace package `{package}` not found"))?;
    let manifest = package.manifest_path.as_std_path();
    if manifest.starts_with(workspace_root.join("os/StarryOS")) {
        Ok(KtestRuntime::Starry)
    } else {
        bail!(
            "ktest currently supports StarryOS and Axvisor packages, got `{}`",
            package.name
        )
    }
}

fn resolve_arch_and_target(
    runtime: KtestRuntime,
    arch: Option<String>,
    target: Option<String>,
) -> anyhow::Result<(String, String)> {
    match runtime {
        KtestRuntime::Starry => resolve_starry_arch_and_target(arch, target),
        KtestRuntime::Axvisor => resolve_axvisor_arch_and_target(arch, target),
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
        KtestRuntime::Starry => starry::build::load_target_from_build_config(build_config),
        KtestRuntime::Axvisor => axvisor::build::load_target_from_build_config(build_config),
    }
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

pub(crate) fn default_qemu_build_config(
    workspace_root: &Path,
    package: &str,
    arch: &str,
) -> PathBuf {
    runtime_config_root(workspace_root, package).join(format!("configs/board/qemu-{arch}.toml"))
}

fn default_qemu_run_config(workspace_root: &Path, package: &str, arch: &str) -> PathBuf {
    runtime_config_root(workspace_root, package).join(format!("configs/qemu/qemu-{arch}.toml"))
}

fn default_board_build_config(workspace_root: &Path, package: &str, board: &str) -> PathBuf {
    runtime_config_root(workspace_root, package).join(format!("configs/board/{board}.toml"))
}

fn default_board_run_config(workspace_root: &Path, package: &str, board: &str) -> PathBuf {
    let root = runtime_config_root(workspace_root, package);
    let starry_board_config = root.join(format!("configs/board/{board}-board.toml"));
    if package != axvisor::build::AXVISOR_PACKAGE && starry_board_config.exists() {
        starry_board_config
    } else {
        root.join(format!("configs/board/{board}.toml"))
    }
}

fn runtime_config_root(workspace_root: &Path, package: &str) -> PathBuf {
    if package == axvisor::build::AXVISOR_PACKAGE {
        workspace_root.join("os/axvisor")
    } else {
        workspace_root.join("os/StarryOS")
    }
}
