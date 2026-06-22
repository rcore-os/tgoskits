use std::{
    env, fs,
    io::Write as _,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Instant,
};

use anyhow::{Context as _, anyhow, bail};
use cargo_metadata::Metadata;
use object::{Object as _, ObjectSection as _};
use ostool::build::config::Cargo;

use super::{Starry, board};
pub type StarryBuildInfo = crate::build::BuildInfo;
pub use crate::build::LogLevel;
use crate::{
    context::{ResolvedStarryRequest, STARRY_PACKAGE, starry_arch_for_target_checked},
    support::process::ProcessExt,
};

pub(crate) fn default_starry_build_info_for_target(target: &str) -> StarryBuildInfo {
    let mut build_info = StarryBuildInfo::default();
    if build_info.effective_plat_dyn(target, None) {
        build_info.features = Vec::new();
    } else {
        build_info.features = vec!["qemu".to_string()];
    }
    build_info
}

pub(crate) fn resolve_build_info_path(
    workspace_root: &Path,
    target: &str,
    explicit_path: Option<PathBuf>,
) -> anyhow::Result<PathBuf> {
    if let Some(path) = explicit_path {
        return Ok(path);
    }

    let _ = starry_arch_for_target_checked(target)?;
    Ok(crate::build::default_build_info_path_in_workspace(
        workspace_root,
        STARRY_PACKAGE,
        target,
    ))
}

pub(crate) fn load_target_from_build_config(path: &Path) -> anyhow::Result<Option<String>> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| anyhow!("failed to read Starry build config {}: {e}", path.display()))?;
    crate::build::reject_removed_std_field(path, &content)?;
    crate::build::reject_arceos_app_c_field(path, &content)?;

    if let Ok(board_file) = toml::from_str::<board::StarryBoardFile>(&content) {
        return Ok(Some(board_file.target));
    }
    if toml::from_str::<StarryBuildInfo>(&content).is_ok() {
        return Ok(None);
    }

    Err(anyhow!("invalid Starry build config {}", path.display()))
}

#[cfg(test)]
pub(crate) fn load_build_info(request: &ResolvedStarryRequest) -> anyhow::Result<StarryBuildInfo> {
    let makefile_features = crate::build::makefile_features_from_env();
    let mut build_info = if let Some(build_info) = &request.build_info_override {
        build_info.clone()
    } else {
        crate::build::ensure_build_info(&request.build_info_path, || {
            default_starry_build_info_for_target(&request.target)
        })?;
        let content = std::fs::read_to_string(&request.build_info_path)?;
        crate::build::reject_arceos_app_c_field(&request.build_info_path, &content)?;
        let build_info: StarryBuildInfo = toml::from_str(&content).with_context(|| {
            format!(
                "failed to parse build info {}",
                request.build_info_path.display()
            )
        })?;
        build_info
    };

    crate::build::apply_makefile_features(&mut build_info, &request.package, &makefile_features);

    if let Some(smp) = request.smp {
        build_info.max_cpu_num = Some(smp);
    }

    Ok(build_info)
}

pub(crate) fn load_cargo_config(request: &ResolvedStarryRequest) -> anyhow::Result<Cargo> {
    let metadata =
        crate::build::cached_workspace_metadata().context("failed to load workspace metadata")?;
    let makefile_features = crate::build::makefile_features_from_env();
    let mut build_info = if let Some(build_info) = &request.build_info_override {
        build_info.clone()
    } else {
        crate::build::ensure_build_info(&request.build_info_path, || {
            default_starry_build_info_for_target(&request.target)
        })?;
        let content = std::fs::read_to_string(&request.build_info_path)?;
        crate::build::reject_arceos_app_c_field(&request.build_info_path, &content)?;
        let build_info: StarryBuildInfo = toml::from_str(&content).with_context(|| {
            format!(
                "failed to parse build info {}",
                request.build_info_path.display()
            )
        })?;
        build_info
    };
    crate::build::apply_makefile_features_with_metadata(
        &mut build_info,
        &request.package,
        &makefile_features,
        metadata,
    );
    normalize_starry_platform_features(&mut build_info.features);
    if let Some(smp) = request.smp {
        build_info.max_cpu_num = Some(smp);
    }
    let plat_dyn = build_info.effective_plat_dyn(&request.target, request.plat_dyn);
    let mut cargo = build_info.into_prepared_base_cargo_config_with_metadata(
        &request.package,
        &request.target,
        request.plat_dyn,
        metadata,
    )?;
    if plat_dyn {
        cargo.features.retain(|feature| {
            !matches!(
                feature.as_str(),
                "ax-feat/plat-dyn" | "ax-std/plat-dyn" | "starry-kernel/plat-dyn"
            )
        });
        cargo.features.push("plat-dyn".to_string());
    }
    patch_starry_cargo_config(&mut cargo, request, metadata)?;
    Ok(cargo)
}

fn normalize_starry_platform_features(features: &mut Vec<String>) {
    let has_sg2002 = features.iter().any(|feature| feature == "sg2002");
    let has_vf2 = features.iter().any(|feature| feature == "vf2");

    if has_sg2002 {
        features.push("ax-hal/riscv64-sg2002".to_string());
    }
    if has_vf2 {
        features.push("ax-hal/riscv64-visionfive2".to_string());
    }

    features.sort();
    features.dedup();
}

fn patch_starry_cargo_config(
    cargo: &mut Cargo,
    request: &ResolvedStarryRequest,
    metadata: &Metadata,
) -> anyhow::Result<()> {
    let platform = crate::context::starry_default_platform_for_arch_checked(&request.arch)?;
    let uses_default_qemu_platform = uses_default_qemu_platform(&cargo.features);

    cargo.package = request.package.clone();
    ensure_starry_bin_arg(&mut cargo.args, &request.package, metadata)?;
    remove_qemu_feature_for_dynamic_platform(cargo);
    if uses_default_qemu_platform {
        cargo.features.push("qemu".to_string());
        cargo.features.sort();
        cargo.features.dedup();
    }

    cargo
        .env
        .insert("AX_ARCH".to_string(), request.arch.clone());
    cargo
        .env
        .insert("AX_TARGET".to_string(), request.target.clone());
    if uses_default_qemu_platform && let Some(platform) = platform {
        cargo
            .env
            .entry("AX_PLATFORM".to_string())
            .or_insert_with(|| platform.to_string());
    }

    if cargo.env.get("UIMAGE").map(|v| v.as_str()) == Some("y") {
        validate_uimage_generation(cargo, &request.arch)?;
    }

    Ok(())
}

fn uimg_arch_for(arch: &str) -> String {
    match arch {
        "aarch64" => "arm64".to_string(),
        "riscv64" => "riscv".to_string(),
        other => other.to_string(),
    }
}

fn validate_uimage_generation(cargo: &Cargo, arch: &str) -> anyhow::Result<()> {
    if cargo.env.contains_key("AX_CONFIG_PATH") {
        return Ok(());
    }

    if !uses_dynamic_platform(&cargo.features) {
        return Err(anyhow::anyhow!(
            "AX_CONFIG_PATH is required for UIMAGE generation"
        ));
    }

    match arch {
        "aarch64" | "riscv64" => Ok(()),
        other => Err(anyhow::anyhow!(
            "AX_CONFIG_PATH is required for UIMAGE generation on {other}"
        )),
    }
}

pub(crate) async fn build_starry_artifact(
    starry: &mut Starry,
    request: &ResolvedStarryRequest,
    cargo: Cargo,
) -> anyhow::Result<ostool::build::CargoBuildOutput> {
    let stage = StageLog::start(format!(
        "starry build package={} target={} arch={}",
        cargo.package, request.target, request.arch
    ));
    let output = starry
        .app
        .build(cargo.clone(), request.build_info_path.clone())
        .await?;
    stage.done();
    postprocess_starry_artifact(starry.app.workspace_root(), request, &cargo, &output)?;
    Ok(output)
}

pub(crate) fn postprocess_starry_artifact(
    workspace_root: &Path,
    request: &ResolvedStarryRequest,
    cargo: &Cargo,
    build_output: &ostool::build::CargoBuildOutput,
) -> anyhow::Result<()> {
    let elf = build_output.elf_path();
    println!("[axbuild] starry artifact elf={}", elf.display());
    generate_kallsyms(elf)?;
    refresh_bin_if_present(elf)?;

    if cargo.env.get("UIMAGE").map(|v| v.as_str()) == Some("y") {
        generate_uimage(workspace_root, cargo, &request.arch, elf)?;
    }

    Ok(())
}

fn generate_kallsyms(kernel_elf: &Path) -> anyhow::Result<()> {
    let stage = StageLog::start(format!("starry kallsyms elf={}", kernel_elf.display()));
    ensure_kallsyms_tools()?;
    let symbols = rust_nm_symbols(kernel_elf)?;
    println!("[axbuild] starry kallsyms symbols={}", symbols.len());
    let mut child = Command::new("gen_ksym")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .context("failed to spawn gen_ksym")?;
    {
        let mut stdin = child
            .stdin
            .take()
            .context("failed to open gen_ksym stdin")?;
        for symbol in symbols {
            writeln!(stdin, "{symbol}").context("failed to write symbols to gen_ksym")?;
        }
    }
    let output = child
        .wait_with_output()
        .context("failed to wait for gen_ksym")?;
    if !output.status.success() {
        bail!("gen_ksym exited with status {}", output.status);
    }

    let section_size = kallsyms_section_size(kernel_elf)?;
    let mut kallsyms = output.stdout;
    if kallsyms.len() > section_size {
        bail!(
            "generated kallsyms ({} bytes) exceed .kallsyms section ({section_size} bytes); \
             remove the stale kernel ELF or rebuild it so the linker script reserve is restored",
            kallsyms.len()
        );
    }
    kallsyms.resize(section_size, 0);

    let temp = temp_file_path(kernel_elf, "kallsyms")?;
    fs::write(&temp, &kallsyms).with_context(|| format!("failed to write {}", temp.display()))?;
    let result = update_kallsyms_section(kernel_elf, &temp);
    let cleanup =
        fs::remove_file(&temp).with_context(|| format!("failed to remove {}", temp.display()));
    result?;
    cleanup?;
    stage.done();
    Ok(())
}

fn rust_nm_symbols(kernel_elf: &Path) -> anyhow::Result<Vec<String>> {
    let output = Command::new("rust-nm")
        .arg("-n")
        .arg(kernel_elf)
        .output()
        .with_context(|| format!("failed to run rust-nm on {}", kernel_elf.display()))?;
    if !output.status.success() {
        bail!("rust-nm exited with status {}", output.status);
    }

    let mut symbols = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let mut fields = line.split_whitespace();
        let Some(address) = fields.next() else {
            continue;
        };
        let Some(kind) = fields.next() else {
            continue;
        };
        let Some(name) = fields.next() else {
            continue;
        };
        if matches!(kind, "T" | "t" | "D" | "B" | "R") && !name.starts_with(".L") && name != "$x" {
            symbols.push(format!("{address} {kind} {name}"));
        }
    }
    Ok(symbols)
}

fn kallsyms_section_size(kernel_elf: &Path) -> anyhow::Result<usize> {
    let data =
        fs::read(kernel_elf).with_context(|| format!("failed to read {}", kernel_elf.display()))?;
    let file = object::File::parse(&*data)
        .with_context(|| format!("failed to parse {}", kernel_elf.display()))?;
    let section = file.section_by_name(".kallsyms").ok_or_else(|| {
        anyhow!(
            "failed to find .kallsyms section in {}",
            kernel_elf.display()
        )
    })?;
    usize::try_from(section.size()).with_context(|| {
        format!(
            ".kallsyms section in {} is too large for this host",
            kernel_elf.display()
        )
    })
}

fn update_kallsyms_section(kernel_elf: &Path, kallsyms: &Path) -> anyhow::Result<()> {
    Command::new("rust-objcopy")
        .arg("--update-section")
        .arg(format!(".kallsyms={}", kallsyms.display()))
        .arg(kernel_elf)
        .exec()
        .with_context(|| format!("failed to update .kallsyms in {}", kernel_elf.display()))
}

fn refresh_bin_if_present(kernel_elf: &Path) -> anyhow::Result<()> {
    let bin = kernel_elf.with_extension("bin");
    if !bin.exists() {
        println!(
            "[axbuild] starry bin refresh skipped: {} does not exist",
            bin.display()
        );
        return Ok(());
    }
    let stage = StageLog::start(format!("starry bin refresh {}", bin.display()));
    Command::new("rust-objcopy")
        .arg("--strip-all")
        .arg("-O")
        .arg("binary")
        .arg(kernel_elf)
        .arg(&bin)
        .exec()
        .with_context(|| format!("failed to refresh {}", bin.display()))?;
    stage.done();
    Ok(())
}

fn generate_uimage(
    workspace_root: &Path,
    cargo: &Cargo,
    arch: &str,
    kernel_elf: &Path,
) -> anyhow::Result<()> {
    let bin = kernel_elf.with_extension("bin");
    if !bin.exists() {
        refresh_bin_if_present(kernel_elf)?;
    }
    if !bin.exists() {
        bail!(
            "kernel BIN is required for UIMAGE generation: {}",
            bin.display()
        );
    }
    let uimg = bin.with_extension("uimg");
    let paddr = uimage_load_paddr(cargo, arch)?;
    let stage = StageLog::start(format!(
        "starry uImage arch={} load={} bin={} out={}",
        arch,
        paddr,
        bin.display(),
        uimg.display()
    ));
    Command::new("mkimage")
        .current_dir(workspace_root)
        .arg("-A")
        .arg(uimg_arch_for(arch))
        .arg("-O")
        .arg("linux")
        .arg("-T")
        .arg("kernel")
        .arg("-C")
        .arg("none")
        .arg("-a")
        .arg(&paddr)
        .arg("-d")
        .arg(&bin)
        .arg(&uimg)
        .exec()
        .with_context(|| format!("failed to generate {}", uimg.display()))?;
    stage.done();
    Ok(())
}

fn ensure_kallsyms_tools() -> anyhow::Result<()> {
    ensure_llvm_tools()?;
    if !command_available("rust-nm") || !command_available("rust-objcopy") {
        install_rust_binutils()?;
    }
    if !command_available("gen_ksym") {
        install_ksym()?;
    }
    require_command("rust-nm")?;
    require_command("rust-objcopy")?;
    require_command("gen_ksym")
}

fn ensure_llvm_tools() -> anyhow::Result<()> {
    if command_available("rust-nm") && command_available("rust-objcopy") {
        return Ok(());
    }
    if !command_available("rustup") {
        return Ok(());
    }
    let output = Command::new("rustup")
        .args(["component", "list", "--installed"])
        .output()
        .context("failed to list installed rustup components")?;
    if String::from_utf8_lossy(&output.stdout)
        .lines()
        .any(|line| line.starts_with("llvm-tools"))
    {
        return Ok(());
    }
    if !kallsyms_auto_install_enabled() {
        bail!(
            "llvm-tools-preview is required; install it with: rustup component add \
             llvm-tools-preview"
        );
    }
    Command::new("rustup")
        .args(["component", "add", "llvm-tools-preview"])
        .exec()
        .context("failed to install llvm-tools-preview")
}

fn install_rust_binutils() -> anyhow::Result<()> {
    if !kallsyms_auto_install_enabled() {
        bail!(
            "rust-nm and rust-objcopy are required; install them with: rustup component add \
             llvm-tools-preview && cargo install cargo-binutils"
        );
    }
    if command_available("rustup") {
        Command::new("rustup")
            .args(["component", "add", "llvm-tools-preview"])
            .exec()
            .context("failed to install llvm-tools-preview")?;
    }
    Command::new("cargo")
        .args(["install", "cargo-binutils"])
        .exec()
        .context("failed to install cargo-binutils")
}

fn install_ksym() -> anyhow::Result<()> {
    if !kallsyms_auto_install_enabled() {
        bail!("gen_ksym is required; install it with: cargo install ksym");
    }
    Command::new("cargo")
        .args(["install", "ksym"])
        .exec()
        .context("failed to install ksym")
}

fn kallsyms_auto_install_enabled() -> bool {
    !matches!(
        env::var("AXBUILD_STARRY_KALLSYMS_AUTO_INSTALL")
            .unwrap_or_else(|_| "1".to_string())
            .as_str(),
        "0" | "n" | "no" | "false" | "off"
    )
}

fn command_available(name: &str) -> bool {
    let path = Path::new(name);
    if path.components().count() > 1 {
        return path.is_file();
    }

    env::var_os("PATH").is_some_and(|paths| {
        env::split_paths(&paths).any(|dir| {
            let candidate = dir.join(name);
            candidate.is_file()
                || cfg!(windows)
                    && env::var_os("PATHEXT").is_some_and(|exts| {
                        exts.to_string_lossy()
                            .split(';')
                            .any(|ext| dir.join(format!("{name}{ext}")).is_file())
                    })
        })
    })
}

fn require_command(name: &str) -> anyhow::Result<()> {
    if command_available(name) {
        Ok(())
    } else {
        bail!("required command `{name}` is not available")
    }
}

fn temp_file_path(path: &Path, suffix: &str) -> anyhow::Result<PathBuf> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("invalid path without parent: {}", path.display()))?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow!("invalid path filename: {}", path.display()))?;
    Ok(parent.join(format!(".{name}.{suffix}.{}.tmp", std::process::id())))
}

fn uimage_load_paddr(cargo: &Cargo, arch: &str) -> anyhow::Result<String> {
    if let Some(config_path) = cargo.env.get("AX_CONFIG_PATH") {
        return axconfig_kernel_base_paddr(Path::new(config_path));
    }

    if !uses_dynamic_platform(&cargo.features) {
        return Err(anyhow::anyhow!(
            "AX_CONFIG_PATH is required for UIMAGE generation"
        ));
    }

    match arch {
        "aarch64" => Ok("0x200000".to_string()),
        "riscv64" => Ok("0x80200000".to_string()),
        other => Err(anyhow::anyhow!(
            "AX_CONFIG_PATH is required for UIMAGE generation on {other}"
        )),
    }
}

fn axconfig_kernel_base_paddr(path: &Path) -> anyhow::Result<String> {
    let output = Command::new("ax-config-gen")
        .arg(path)
        .arg("-r")
        .arg("plat.kernel-base-paddr")
        .output()
        .with_context(|| format!("failed to read kernel base paddr from {}", path.display()))?;
    if !output.status.success() {
        bail!("ax-config-gen exited with status {}", output.status);
    }
    let paddr = String::from_utf8_lossy(&output.stdout)
        .trim()
        .replace('_', "");
    if paddr.is_empty() {
        bail!(
            "ax-config-gen returned empty plat.kernel-base-paddr for {}",
            path.display()
        );
    }
    Ok(paddr)
}

fn remove_qemu_feature_for_dynamic_platform(cargo: &mut Cargo) {
    if uses_dynamic_platform(&cargo.features) {
        cargo.features.retain(|feature| feature != "qemu");
    }
}

fn uses_dynamic_platform(features: &[String]) -> bool {
    features.iter().any(|feature| {
        matches!(
            feature.as_str(),
            "plat-dyn"
                | "ax-feat/plat-dyn"
                | "ax-std/plat-dyn"
                | "starry-kernel/plat-dyn"
                | "ax-hal/plat-dyn"
        )
    })
}

fn uses_default_qemu_platform(features: &[String]) -> bool {
    let has_static_platform = features.iter().any(|feature| {
        matches!(
            feature.as_str(),
            "defplat" | "ax-feat/defplat" | "ax-std/defplat"
        ) || default_starry_qemu_platform_feature(feature).is_some()
    });
    let has_dynamic = uses_dynamic_platform(features);
    let has_custom = features.iter().any(|feature| {
        matches!(
            feature.as_str(),
            "myplat" | "ax-feat/myplat" | "ax-std/myplat" | "ax-hal/myplat"
        )
    });

    has_static_platform && !has_dynamic && !has_custom
}

fn default_starry_qemu_platform_feature(feature: &str) -> Option<&str> {
    match feature.strip_prefix("ax-hal/")? {
        "loongarch64-qemu-virt" => Some(feature),
        _ => None,
    }
}

fn ensure_starry_bin_arg(
    args: &mut Vec<String>,
    package: &str,
    metadata: &Metadata,
) -> anyhow::Result<()> {
    if args.iter().any(|arg| arg == "--bin") {
        return Ok(());
    }

    if package_has_bin_named(package, package, metadata)? {
        args.push("--bin".to_string());
        args.push(package.to_string());
    }

    Ok(())
}

fn package_has_bin_named(
    package: &str,
    bin_name: &str,
    metadata: &Metadata,
) -> anyhow::Result<bool> {
    let package_info = metadata
        .packages
        .iter()
        .find(|pkg| metadata.workspace_members.contains(&pkg.id) && pkg.name == package)
        .ok_or_else(|| anyhow::anyhow!("workspace package `{package}` not found"))?;

    Ok(package_info.targets.iter().any(|target| {
        target.name == bin_name
            && target
                .kind
                .iter()
                .any(|kind| matches!(kind, cargo_metadata::TargetKind::Bin))
    }))
}

struct StageLog {
    name: String,
    started: Instant,
}

impl StageLog {
    fn start(name: impl Into<String>) -> Self {
        let name = name.into();
        println!("[axbuild] {name} ...");
        Self {
            name,
            started: Instant::now(),
        }
    }

    fn done(self) {
        println!(
            "[axbuild] {} ... done ({:.2?})",
            self.name,
            self.started.elapsed()
        );
    }
}

#[cfg(test)]
mod tests;
