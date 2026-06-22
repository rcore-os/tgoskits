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
mod tests {
    use std::{collections::HashMap, fs};

    use tempfile::tempdir;

    use super::*;
    use crate::context::STARRY_PACKAGE;

    fn write_minimal_package_manifest(path: &Path, name: &str) {
        let src_dir = path.parent().unwrap().join("src");
        fs::create_dir_all(&src_dir).unwrap();
        fs::write(src_dir.join("lib.rs"), "").unwrap();
        fs::write(
            path,
            format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n"),
        )
        .unwrap();
    }

    fn request(path: PathBuf, arch: &str, target: &str) -> ResolvedStarryRequest {
        ResolvedStarryRequest {
            package: STARRY_PACKAGE.to_string(),
            arch: arch.to_string(),
            target: target.to_string(),
            plat_dyn: None,
            smp: None,
            debug: false,
            build_info_path: path,
            build_info_override: None,
            qemu_config: None,
            uboot_config: None,
        }
    }

    #[test]
    fn resolve_build_info_path_uses_default_starry_location() {
        let root = tempdir().unwrap();
        let starry_dir = root.path().join("os/StarryOS/starryos");
        fs::create_dir_all(&starry_dir).unwrap();
        write_minimal_package_manifest(&starry_dir.join("Cargo.toml"), STARRY_PACKAGE);
        fs::write(
            root.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"os/StarryOS/starryos\"]\n",
        )
        .unwrap();
        let path =
            resolve_build_info_path(root.path(), "aarch64-unknown-none-softfloat", None).unwrap();

        assert_eq!(
            path,
            root.path()
                .join("tmp/axbuild/config/starryos/build-aarch64-unknown-none-softfloat.toml")
        );
    }

    #[test]
    fn resolve_build_info_path_ignores_source_tree_defaults() {
        let root = tempdir().unwrap();
        let starry_dir = root.path().join("os/StarryOS/starryos");
        fs::create_dir_all(&starry_dir).unwrap();
        write_minimal_package_manifest(&starry_dir.join("Cargo.toml"), STARRY_PACKAGE);
        fs::write(
            root.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"os/StarryOS/starryos\"]\n",
        )
        .unwrap();
        let bare = starry_dir.join("build-aarch64-unknown-none-softfloat.toml");
        let dotted = starry_dir.join(".build-aarch64-unknown-none-softfloat.toml");
        fs::write(&bare, "").unwrap();
        fs::write(&dotted, "").unwrap();

        let path =
            resolve_build_info_path(root.path(), "aarch64-unknown-none-softfloat", None).unwrap();

        assert_eq!(
            path,
            root.path()
                .join("tmp/axbuild/config/starryos/build-aarch64-unknown-none-softfloat.toml")
        );
    }

    #[test]
    fn resolve_build_info_path_prefers_explicit_path() {
        let root = tempdir().unwrap();
        let starry_dir = root.path().join("os/StarryOS/starryos");
        fs::create_dir_all(&starry_dir).unwrap();
        write_minimal_package_manifest(&starry_dir.join("Cargo.toml"), STARRY_PACKAGE);
        fs::write(
            root.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"os/StarryOS/starryos\"]\n",
        )
        .unwrap();
        let explicit = root.path().join("custom/build.toml");
        let path =
            resolve_build_info_path(root.path(), "x86_64-unknown-none", Some(explicit.clone()))
                .unwrap();

        assert_eq!(path, explicit);
    }

    #[test]
    fn load_build_info_writes_default_template_when_missing() {
        let root = tempdir().unwrap();
        let path = root.path().join(".build-target.toml");
        let request = request(path.clone(), "aarch64", "aarch64-unknown-none-softfloat");

        let build_info = load_build_info(&request).unwrap();

        assert_eq!(
            build_info,
            default_starry_build_info_for_target("aarch64-unknown-none-softfloat")
        );
        assert!(path.exists());
        let persisted: StarryBuildInfo =
            toml::from_str(&fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(persisted, build_info);
    }

    #[test]
    fn default_aarch64_starry_build_info_uses_dynamic_platform() {
        let build_info = default_starry_build_info_for_target("aarch64-unknown-none-softfloat");

        assert!(build_info.plat_dyn);
        assert!(!build_info.features.contains(&"qemu".to_string()));
    }

    #[test]
    fn default_riscv64_starry_build_info_uses_dynamic_platform() {
        let build_info = default_starry_build_info_for_target("riscv64gc-unknown-none-elf");

        assert!(build_info.plat_dyn);
        assert!(!build_info.features.contains(&"qemu".to_string()));
    }

    #[test]
    fn default_x86_starry_build_info_uses_dynamic_platform() {
        let build_info = default_starry_build_info_for_target("x86_64-unknown-none");

        assert!(build_info.plat_dyn);
        assert!(build_info.features.is_empty());
    }

    #[test]
    fn load_build_info_reads_existing_file() {
        let root = tempdir().unwrap();
        let path = root.path().join(".build-target.toml");
        fs::write(
            &path,
            r#"
log = "Info"
features = ["net"]

[env]
HELLO = "world"
"#,
        )
        .unwrap();

        let request = request(path, "aarch64", "aarch64-unknown-none-softfloat");
        let build_info = load_build_info(&request).unwrap();

        assert_eq!(build_info.log, LogLevel::Info);
        assert_eq!(build_info.features, vec!["net".to_string()]);
        assert_eq!(
            build_info.env.get("HELLO").map(String::as_str),
            Some("world")
        );
    }

    #[test]
    fn load_target_from_build_config_rejects_removed_std_field() {
        let root = tempdir().unwrap();
        let path = root.path().join(".build-target.toml");
        fs::write(
            &path,
            r#"
std = false
features = []
log = "Info"

"#,
        )
        .unwrap();

        let err = load_target_from_build_config(&path).unwrap_err();

        assert!(
            err.to_string().contains("uses removed `std` field"),
            "{err:#}"
        );
    }

    #[test]
    fn load_target_from_build_config_rejects_arceos_app_c_field() {
        let root = tempdir().unwrap();
        let path = root.path().join(".build-target.toml");
        fs::write(
            &path,
            r#"
app-c = "c"
features = []
log = "Info"

"#,
        )
        .unwrap();

        let err = load_target_from_build_config(&path).unwrap_err();

        assert!(
            err.to_string().contains("uses ArceOS-only `app-c` field"),
            "{err:#}"
        );
    }

    #[test]
    fn load_build_info_prefers_request_override_without_writing_file() {
        let root = tempdir().unwrap();
        let path = root.path().join(".build-target.toml");
        let mut request = request(path.clone(), "aarch64", "aarch64-unknown-none-softfloat");
        request.build_info_override = Some(StarryBuildInfo {
            log: LogLevel::Info,
            features: vec!["net".to_string()],
            ..default_starry_build_info_for_target("aarch64-unknown-none-softfloat")
        });

        let build_info = load_build_info(&request).unwrap();

        assert_eq!(build_info.log, LogLevel::Info);
        assert_eq!(build_info.features, vec!["net".to_string()]);
        assert!(!path.exists());
    }

    #[test]
    fn patch_starry_cargo_config_injects_required_features_and_env() {
        let request = request(
            PathBuf::from("/tmp/.build.toml"),
            "aarch64",
            "aarch64-unknown-none-softfloat",
        );
        let build_info = StarryBuildInfo {
            env: HashMap::from([(String::from("CUSTOM"), String::from("1"))]),
            features: vec!["net".to_string()],
            log: LogLevel::Info,
            max_cpu_num: None,
            axconfig_overrides: Vec::new(),
            plat_dyn: false,
        };
        let mut cargo = build_info.into_base_cargo_config_with_log(
            STARRY_PACKAGE.to_string(),
            request.target.clone(),
            vec![],
        );
        let metadata = crate::build::workspace_metadata().unwrap();
        patch_starry_cargo_config(&mut cargo, &request, &metadata).unwrap();

        assert_eq!(cargo.package, STARRY_PACKAGE);
        assert_eq!(cargo.target, "aarch64-unknown-none-softfloat");
        assert_eq!(cargo.features, vec!["net".to_string()]);
        assert_eq!(
            cargo.env.get("AX_ARCH").map(String::as_str),
            Some("aarch64")
        );
        assert_eq!(
            cargo.env.get("AX_TARGET").map(String::as_str),
            Some("aarch64-unknown-none-softfloat")
        );
        assert_eq!(cargo.env.get("AX_PLATFORM").map(String::as_str), None);
        assert_eq!(cargo.env.get("AX_LOG").map(String::as_str), Some("info"));
        assert_eq!(cargo.env.get("CUSTOM").map(String::as_str), Some("1"));
        assert!(cargo.to_bin);
        assert!(cargo.post_build_cmds.is_empty());
    }

    #[test]
    fn patch_starry_cargo_config_preserves_request_package() {
        let request = ResolvedStarryRequest {
            package: STARRY_PACKAGE.to_string(),
            arch: "x86_64".to_string(),
            target: "x86_64-unknown-none".to_string(),
            plat_dyn: None,
            smp: None,
            debug: false,
            build_info_path: PathBuf::from("/tmp/.build.toml"),
            build_info_override: None,
            qemu_config: None,
            uboot_config: None,
        };
        let build_info = default_starry_build_info_for_target("x86_64-unknown-none");
        let mut cargo = build_info.into_base_cargo_config_with_log(
            "placeholder".to_string(),
            request.target.clone(),
            vec![],
        );

        let metadata = crate::build::workspace_metadata().unwrap();
        patch_starry_cargo_config(&mut cargo, &request, &metadata).unwrap();

        assert_eq!(cargo.package, STARRY_PACKAGE);
        assert_eq!(
            cargo.args,
            vec!["--bin".to_string(), STARRY_PACKAGE.to_string()]
        );
    }

    #[test]
    fn patch_starry_cargo_config_skips_qemu_for_dynamic_platforms() {
        let request = request(
            PathBuf::from("/tmp/.build.toml"),
            "aarch64",
            "aarch64-unknown-none-softfloat",
        );
        let build_info = StarryBuildInfo {
            env: HashMap::new(),
            features: vec![
                "common".to_string(),
                "plat-dyn".to_string(),
                "ax-driver/rockchip-soc".to_string(),
                "ax-driver/rockchip-sdhci".to_string(),
            ],
            log: LogLevel::Info,
            max_cpu_num: Some(8),
            axconfig_overrides: Vec::new(),
            plat_dyn: true,
        };
        let mut cargo = build_info.into_base_cargo_config_with_log(
            STARRY_PACKAGE.to_string(),
            "scripts/targets/std/pie/aarch64-unknown-linux-musl.json".to_string(),
            Vec::new(),
        );

        let metadata = crate::build::workspace_metadata().unwrap();
        patch_starry_cargo_config(&mut cargo, &request, &metadata).unwrap();

        assert!(
            cargo
                .features
                .contains(&"ax-driver/rockchip-soc".to_string())
        );
        assert!(
            cargo
                .features
                .contains(&"ax-driver/rockchip-sdhci".to_string())
        );
        assert!(!cargo.features.contains(&"qemu".to_string()));
        assert!(!cargo.env.contains_key("AX_PLATFORM"));
        assert_eq!(
            cargo.target,
            "scripts/targets/std/pie/aarch64-unknown-linux-musl.json"
        );
    }

    #[test]
    fn patch_starry_cargo_config_removes_qemu_for_dynamic_platforms() {
        let request = request(
            PathBuf::from("/tmp/.build.toml"),
            "aarch64",
            "aarch64-unknown-none-softfloat",
        );
        let build_info = StarryBuildInfo {
            env: HashMap::new(),
            features: vec!["qemu".to_string(), "plat-dyn".to_string()],
            log: LogLevel::Info,
            max_cpu_num: None,
            axconfig_overrides: Vec::new(),
            plat_dyn: true,
        };
        let mut cargo = build_info.into_base_cargo_config_with_log(
            STARRY_PACKAGE.to_string(),
            "scripts/targets/std/pie/aarch64-unknown-linux-musl.json".to_string(),
            Vec::new(),
        );

        let metadata = crate::build::workspace_metadata().unwrap();
        patch_starry_cargo_config(&mut cargo, &request, &metadata).unwrap();

        assert!(!cargo.features.contains(&"qemu".to_string()));
        assert!(!cargo.env.contains_key("AX_PLATFORM"));
    }

    #[test]
    fn aarch64_qemu_virt_is_not_a_default_static_starry_platform() {
        assert_eq!(
            default_starry_qemu_platform_feature("ax-hal/aarch64-qemu-virt"),
            None
        );
    }

    #[test]
    fn aarch64_has_no_static_starry_default_platform() {
        assert_eq!(
            crate::context::starry_default_platform_for_arch_checked("aarch64").unwrap(),
            None
        );
    }

    #[test]
    fn riscv64_has_no_starry_default_platform() {
        assert_eq!(
            crate::context::starry_default_platform_for_arch_checked("riscv64").unwrap(),
            None
        );
    }

    #[test]
    fn uimage_load_paddr_uses_dynamic_riscv64_fallback_without_axconfig() {
        let cargo = Cargo {
            env: HashMap::new(),
            target: "scripts/targets/std/pie/riscv64gc-unknown-linux-musl.json".to_string(),
            package: STARRY_PACKAGE.to_string(),
            bin: None,
            features: vec!["plat-dyn".to_string()],
            log: None,
            extra_config: None,
            profile: None,
            disable_someboot_build_config: true,
            args: Vec::new(),
            pre_build_cmds: Vec::new(),
            post_build_cmds: Vec::new(),
            to_bin: true,
        };

        assert_eq!(uimage_load_paddr(&cargo, "riscv64").unwrap(), "0x80200000");
    }

    #[test]
    fn uimage_load_paddr_uses_axconfig_when_available() {
        let cargo = Cargo {
            env: HashMap::from([(
                "AX_CONFIG_PATH".to_string(),
                "/tmp/generated.axconfig.toml".to_string(),
            )]),
            target: "riscv64gc-unknown-none-elf".to_string(),
            package: STARRY_PACKAGE.to_string(),
            bin: None,
            features: vec!["plat-dyn".to_string()],
            log: None,
            extra_config: None,
            profile: None,
            disable_someboot_build_config: true,
            args: Vec::new(),
            pre_build_cmds: Vec::new(),
            post_build_cmds: Vec::new(),
            to_bin: true,
        };

        let err = uimage_load_paddr(&cargo, "riscv64").unwrap_err();
        assert!(err.to_string().contains("ax-config-gen"));
    }

    #[test]
    fn load_cargo_config_treats_sg2002_as_explicit_platform_feature() {
        let mut request = request(
            PathBuf::from("/tmp/.build.toml"),
            "riscv64",
            "riscv64gc-unknown-none-elf",
        );
        request.build_info_override = Some(StarryBuildInfo {
            features: vec!["sg2002".to_string()],
            plat_dyn: false,
            ..default_starry_build_info_for_target("riscv64gc-unknown-none-elf")
        });

        let cargo = load_cargo_config(&request).unwrap();

        assert!(cargo.features.contains(&"sg2002".to_string()));
        assert!(
            cargo
                .features
                .contains(&"ax-hal/riscv64-sg2002".to_string())
        );
        assert!(
            !cargo
                .features
                .iter()
                .any(|feature| feature.starts_with("qemu"))
        );
        assert!(!cargo.features.contains(&"qemu".to_string()));
        assert_eq!(
            cargo.env.get("AX_PLATFORM").map(String::as_str),
            Some("riscv64-sg2002")
        );
    }

    #[test]
    fn load_cargo_config_keeps_pie_target_for_non_kmod_plat_dyn_request() {
        let mut request = request(
            PathBuf::from("/tmp/.build.toml"),
            "aarch64",
            "aarch64-unknown-none-softfloat",
        );
        request.build_info_override = Some(StarryBuildInfo {
            plat_dyn: true,
            features: vec!["ax-driver/virtio-blk".to_string()],
            ..default_starry_build_info_for_target("aarch64-unknown-none-softfloat")
        });

        let cargo = load_cargo_config(&request).unwrap();

        assert!(
            cargo
                .target
                .ends_with("scripts/targets/std/pie/aarch64-unknown-linux-musl.json"),
            "expected pie target, got {}",
            cargo.target
        );
    }

    #[test]
    fn resolve_build_info_path_supports_starry_subworkspace_root() {
        let root = tempdir().unwrap();
        let starry_dir = root.path().join("starryos");
        fs::create_dir_all(&starry_dir).unwrap();
        write_minimal_package_manifest(&starry_dir.join("Cargo.toml"), STARRY_PACKAGE);
        fs::write(
            root.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"starryos\"]\n",
        )
        .unwrap();

        let path =
            resolve_build_info_path(root.path(), "aarch64-unknown-none-softfloat", None).unwrap();

        assert_eq!(
            path,
            root.path()
                .join("tmp/axbuild/config/starryos/build-aarch64-unknown-none-softfloat.toml")
        );
    }

    #[test]
    fn patch_starry_cargo_config_preserves_json_target() {
        let request = ResolvedStarryRequest {
            package: STARRY_PACKAGE.to_string(),
            arch: "aarch64".to_string(),
            target: "aarch64-unknown-none-softfloat".to_string(),
            plat_dyn: None,
            smp: None,
            debug: false,
            build_info_path: PathBuf::from(
                "/tmp/os/StarryOS/starryos/.build-aarch64-unknown-none-softfloat.toml",
            ),
            build_info_override: None,
            qemu_config: None,
            uboot_config: None,
        };
        let build_info = default_starry_build_info_for_target(&request.target);
        let mut cargo = build_info.into_base_cargo_config_with_log(
            request.package.clone(),
            "scripts/targets/std/aarch64-unknown-linux-musl.json".to_string(),
            vec!["-Z".to_string(), "json-target-spec".to_string()],
        );

        let metadata = crate::build::workspace_metadata().unwrap();
        patch_starry_cargo_config(&mut cargo, &request, &metadata).unwrap();

        assert_eq!(
            cargo.target,
            "scripts/targets/std/aarch64-unknown-linux-musl.json"
        );
        assert_eq!(cargo.env.get("AX_TARGET"), Some(&request.target));
    }

    #[test]
    fn ensure_starry_bin_arg_adds_bin_for_starryos_package() {
        let mut args = Vec::new();

        let metadata = crate::build::workspace_metadata().unwrap();
        ensure_starry_bin_arg(&mut args, "starryos", &metadata).unwrap();

        assert_eq!(args, vec!["--bin".to_string(), "starryos".to_string()]);
    }

    #[test]
    fn ensure_starry_bin_arg_keeps_existing_bin_arg() {
        let mut args = vec!["--bin".to_string(), "starryos".to_string()];

        let metadata = crate::build::workspace_metadata().unwrap();
        ensure_starry_bin_arg(&mut args, STARRY_PACKAGE, &metadata).unwrap();

        assert_eq!(args, vec!["--bin".to_string(), "starryos".to_string()]);
    }

    #[test]
    fn patch_starry_cargo_config_validates_uimage_without_shell_post_build() {
        let request = request(
            PathBuf::from("/tmp/.build.toml"),
            "riscv64",
            "riscv64gc-unknown-none-elf",
        );
        let mut build_info = default_starry_build_info_for_target(&request.target);
        build_info.env.insert("UIMAGE".to_string(), "y".to_string());
        let mut cargo = build_info.into_base_cargo_config_with_log(
            request.package.clone(),
            request.target.clone(),
            StarryBuildInfo::build_cargo_args(&request.target, &[]),
        );
        cargo.features.push("plat-dyn".to_string());

        let metadata = crate::build::workspace_metadata().unwrap();
        patch_starry_cargo_config(&mut cargo, &request, &metadata).unwrap();

        assert!(cargo.post_build_cmds.is_empty());
    }
}
