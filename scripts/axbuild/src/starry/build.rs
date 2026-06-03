use std::path::{Path, PathBuf};

use anyhow::{Context as _, anyhow};
use cargo_metadata::Metadata;
use ostool::build::config::Cargo;

use super::board;
pub type StarryBuildInfo = crate::build::BuildInfo;
pub use crate::build::LogLevel;
use crate::context::{ResolvedStarryRequest, STARRY_PACKAGE, starry_arch_for_target_checked};

pub(crate) fn default_starry_build_info_for_target(target: &str) -> StarryBuildInfo {
    let mut build_info = StarryBuildInfo::default_for_target(target);
    if build_info.plat_dyn {
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
        let mut build_info: StarryBuildInfo = toml::from_str(&content).with_context(|| {
            format!(
                "failed to parse build info {}",
                request.build_info_path.display()
            )
        })?;
        crate::build::apply_target_defaults_if_plat_dyn_unspecified(
            &mut build_info,
            &request.target,
            &content,
        );
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
        let mut build_info: StarryBuildInfo = toml::from_str(&content).with_context(|| {
            format!(
                "failed to parse build info {}",
                request.build_info_path.display()
            )
        })?;
        crate::build::apply_target_defaults_if_plat_dyn_unspecified(
            &mut build_info,
            &request.target,
            &content,
        );
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

    inject_kallsyms_pre_build_cmd(cargo, &request.target)?;
    inject_kallsyms_post_build_cmd(cargo)?;

    if cargo.env.get("UIMAGE").map(|v| v.as_str()) == Some("y") {
        inject_uimage_post_build_cmd(cargo, &request.arch)?;
    }

    Ok(())
}

fn inject_kallsyms_pre_build_cmd(cargo: &mut Cargo, target: &str) -> anyhow::Result<()> {
    let target_dir = crate::context::workspace_root_path()?
        .join("target")
        .join(target)
        .join("release");
    let kernel = target_dir.join(STARRY_PACKAGE);
    let bin = target_dir.join(format!("{}.bin", STARRY_PACKAGE));
    let cmd = format!(
        "rm -f {} {}",
        shell_quote(&kernel.display().to_string()),
        shell_quote(&bin.display().to_string())
    );
    cargo.pre_build_cmds.push(cmd);
    Ok(())
}

fn inject_kallsyms_post_build_cmd(cargo: &mut Cargo) -> anyhow::Result<()> {
    let script = crate::context::workspace_root_path()?
        .join("scripts")
        .join("axbuild")
        .join("scripts")
        .join("starry-kallsyms.sh");
    cargo
        .post_build_cmds
        .push(format!("sh {}", shell_quote(&script.display().to_string())));
    Ok(())
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn uimg_arch_for(arch: &str) -> String {
    match arch {
        "aarch64" => "arm64".to_string(),
        "riscv64" => "riscv".to_string(),
        other => other.to_string(),
    }
}

fn inject_uimage_post_build_cmd(cargo: &mut Cargo, arch: &str) -> anyhow::Result<()> {
    let uimg_arch = uimg_arch_for(arch);
    let paddr = uimage_load_paddr_expr(cargo, arch)?;

    let cmd = format!(
        "paddr={paddr} && bin=${{KERNEL_ELF%.elf}}.bin && mkimage -A {uimg_arch} -O linux -T \
         kernel -C none -a \"$paddr\" -d \"$bin\" \"${{bin%.bin}}.uimg\""
    );
    cargo.post_build_cmds.push(cmd);
    Ok(())
}

fn uimage_load_paddr_expr(cargo: &Cargo, arch: &str) -> anyhow::Result<String> {
    if let Some(config_path) = cargo.env.get("AX_CONFIG_PATH") {
        return Ok(format!(
            "$(ax-config-gen {config_path} -r plat.kernel-base-paddr | tr -d _)"
        ));
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
        "x86-pc" | "loongarch64-qemu-virt" => Some(feature),
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
    fn default_x86_starry_build_info_keeps_static_qemu_feature() {
        let build_info = default_starry_build_info_for_target("x86_64-unknown-none");

        assert!(!build_info.plat_dyn);
        assert_eq!(build_info.features, vec!["qemu".to_string()]);
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
            std_build: false,
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
        assert_eq!(cargo.post_build_cmds.len(), 1);
        assert!(cargo.post_build_cmds[0].contains("scripts/axbuild/scripts/starry-kallsyms.sh"));
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
            std_build: false,
        };
        let mut cargo = build_info.into_base_cargo_config_with_log(
            STARRY_PACKAGE.to_string(),
            "scripts/targets/pie/aarch64-unknown-none-softfloat.json".to_string(),
            StarryBuildInfo::build_cargo_args(
                "scripts/targets/pie/aarch64-unknown-none-softfloat.json",
                &[],
            ),
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
            "scripts/targets/pie/aarch64-unknown-none-softfloat.json"
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
            std_build: false,
        };
        let mut cargo = build_info.into_base_cargo_config_with_log(
            STARRY_PACKAGE.to_string(),
            "scripts/targets/pie/aarch64-unknown-none-softfloat.json".to_string(),
            StarryBuildInfo::build_cargo_args(
                "scripts/targets/pie/aarch64-unknown-none-softfloat.json",
                &[],
            ),
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
            target: "scripts/targets/pie/riscv64gc-unknown-none-elf.json".to_string(),
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

        assert_eq!(
            uimage_load_paddr_expr(&cargo, "riscv64").unwrap(),
            "0x80200000"
        );
    }

    #[test]
    fn uimage_load_paddr_prefers_axconfig_when_available() {
        let mut cargo = Cargo {
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

        assert_eq!(
            uimage_load_paddr_expr(&cargo, "riscv64").unwrap(),
            "$(ax-config-gen /tmp/generated.axconfig.toml -r plat.kernel-base-paddr | tr -d _)"
        );

        inject_uimage_post_build_cmd(&mut cargo, "riscv64").unwrap();
        assert!(
            cargo.post_build_cmds[0].contains("paddr=$(ax-config-gen /tmp/generated.axconfig.toml")
        );
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
            "scripts/targets/no-pie/aarch64-unknown-none-softfloat.json".to_string(),
            StarryBuildInfo::build_cargo_args(
                "scripts/targets/no-pie/aarch64-unknown-none-softfloat.json",
                &[],
            ),
        );

        let metadata = crate::build::workspace_metadata().unwrap();
        patch_starry_cargo_config(&mut cargo, &request, &metadata).unwrap();

        assert_eq!(
            cargo.target,
            "scripts/targets/no-pie/aarch64-unknown-none-softfloat.json"
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
    fn patch_starry_cargo_config_runs_kallsyms_before_uimage_generation() {
        let request = request(
            PathBuf::from("/tmp/.build.toml"),
            "aarch64",
            "aarch64-unknown-none-softfloat",
        );
        let mut build_info = default_starry_build_info_for_target(&request.target);
        build_info.env.insert("UIMAGE".to_string(), "y".to_string());
        build_info.env.insert(
            "AX_CONFIG_PATH".to_string(),
            "/tmp/.axconfig.toml".to_string(),
        );
        let mut cargo = build_info.into_base_cargo_config_with_log(
            request.package.clone(),
            request.target.clone(),
            StarryBuildInfo::build_cargo_args(&request.target, &[]),
        );

        let metadata = crate::build::workspace_metadata().unwrap();
        patch_starry_cargo_config(&mut cargo, &request, &metadata).unwrap();

        assert_eq!(cargo.post_build_cmds.len(), 2);
        assert!(cargo.post_build_cmds[0].contains("scripts/axbuild/scripts/starry-kallsyms.sh"));
        assert!(cargo.post_build_cmds[1].contains("mkimage"));
    }
    #[test]
    fn starry_kallsyms_script_does_not_require_gawk_extensions() {
        let script = crate::context::workspace_root_path()
            .unwrap()
            .join("scripts/axbuild/scripts/starry-kallsyms.sh");
        let content = fs::read_to_string(script).unwrap();

        assert!(
            !content.contains("strtonum("),
            "starry-kallsyms.sh must run with non-gawk awk implementations used by CI"
        );
    }
}
