use std::path::{Path, PathBuf};

use ostool::build::config::Cargo;

pub type StarryBuildInfo = crate::arceos::build::ArceosBuildInfo;
pub use crate::arceos::build::LogLevel;
use crate::context::{
    ResolvedStarryRequest, STARRY_PACKAGE, starry_arch_for_target_checked, workspace_manifest_path,
    workspace_member_dir_in, workspace_metadata_root_manifest,
};

impl StarryBuildInfo {
    pub fn default_starry_for_target(target: &str) -> Self {
        let mut build_info = Self::default_for_target(target);
        build_info.plat_dyn = false;
        build_info.features = vec!["qemu".to_string()];
        build_info
    }
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
    Ok(crate::arceos::build::resolve_build_info_path_in_dir(
        &workspace_member_dir_in(workspace_root, STARRY_PACKAGE)?,
        target,
    ))
}

pub(crate) fn load_build_info(request: &ResolvedStarryRequest) -> anyhow::Result<StarryBuildInfo> {
    let makefile_features = crate::arceos::build::makefile_features_from_env();
    load_build_info_with_makefile_features(request, &makefile_features)
}

fn load_build_info_with_makefile_features(
    request: &ResolvedStarryRequest,
    makefile_features: &[String],
) -> anyhow::Result<StarryBuildInfo> {
    let mut build_info = if let Some(build_info) = &request.build_info_override {
        build_info.clone()
    } else {
        crate::arceos::build::load_or_create_build_info(&request.build_info_path, || {
            StarryBuildInfo::default_starry_for_target(&request.target)
        })?
    };

    if let Some(smp) = request.smp {
        build_info.max_cpu_num = Some(smp);
    }

    crate::arceos::build::apply_makefile_features(
        &mut build_info,
        &request.package,
        makefile_features,
    );

    Ok(build_info)
}

pub(crate) fn load_cargo_config(request: &ResolvedStarryRequest) -> anyhow::Result<Cargo> {
    to_cargo_config(load_build_info(request)?, request)
}

pub(crate) fn to_cargo_config(
    build_info: StarryBuildInfo,
    request: &ResolvedStarryRequest,
) -> anyhow::Result<Cargo> {
    let mut cargo = build_info.into_prepared_base_cargo_config(
        &request.package,
        &request.target,
        request.plat_dyn,
    )?;
    patch_starry_cargo_config(&mut cargo, request)?;
    Ok(cargo)
}

fn patch_starry_cargo_config(
    cargo: &mut Cargo,
    request: &ResolvedStarryRequest,
) -> anyhow::Result<()> {
    let platform = default_platform_for_arch(&request.arch)?;
    let static_defplat = uses_static_default_platform(&cargo.features);

    cargo.package = request.package.clone();
    cargo.target = request.target.clone();
    ensure_starry_bin_arg(&mut cargo.args, &request.package)?;
    if static_defplat {
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
    if static_defplat {
        cargo
            .env
            .entry("AX_PLATFORM".to_string())
            .or_insert_with(|| platform.to_string());
    }

    Ok(())
}

fn uses_static_default_platform(features: &[String]) -> bool {
    let has_defplat = features.iter().any(|feature| {
        matches!(
            feature.as_str(),
            "defplat" | "ax-feat/defplat" | "ax-std/defplat"
        )
    });
    let has_dynamic = features.iter().any(|feature| {
        matches!(
            feature.as_str(),
            "plat-dyn" | "ax-feat/plat-dyn" | "ax-std/plat-dyn"
        )
    });
    let has_custom = features.iter().any(|feature| {
        matches!(
            feature.as_str(),
            "myplat" | "ax-feat/myplat" | "ax-std/myplat"
        )
    });

    has_defplat && !has_dynamic && !has_custom
}

fn ensure_starry_bin_arg(args: &mut Vec<String>, package: &str) -> anyhow::Result<()> {
    if args.iter().any(|arg| arg == "--bin") {
        return Ok(());
    }

    if package_has_bin_named(package, package)? {
        args.push("--bin".to_string());
        args.push(package.to_string());
    }

    Ok(())
}

fn package_has_bin_named(package: &str, bin_name: &str) -> anyhow::Result<bool> {
    let workspace_manifest = workspace_manifest_path()?;
    let metadata = workspace_metadata_root_manifest(&workspace_manifest)?;
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

fn default_platform_for_arch(arch: &str) -> anyhow::Result<&'static str> {
    match arch {
        "aarch64" => Ok("aarch64-qemu-virt"),
        "x86_64" => Ok("x86-pc"),
        "riscv64" => Ok("riscv64-qemu-virt"),
        "loongarch64" => Ok("loongarch64-qemu-virt"),
        _ => anyhow::bail!(
            "unsupported Starry architecture `{arch}`; expected one of aarch64, x86_64, riscv64, \
             loongarch64"
        ),
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
                .join("os/StarryOS/starryos/.build-aarch64-unknown-none-softfloat.toml")
        );
    }

    #[test]
    fn resolve_build_info_path_prefers_existing_bare_name() {
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

        assert_eq!(path, bare);
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
            StarryBuildInfo::default_starry_for_target("aarch64-unknown-none-softfloat")
        );
        assert!(path.exists());
        let persisted: StarryBuildInfo =
            toml::from_str(&fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(persisted, build_info);
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
            ..StarryBuildInfo::default_starry_for_target("aarch64-unknown-none-softfloat")
        });

        let build_info = load_build_info(&request).unwrap();

        assert_eq!(build_info.log, LogLevel::Info);
        assert_eq!(build_info.features, vec!["net".to_string()]);
        assert!(!path.exists());
    }

    #[test]
    fn load_build_info_with_makefile_features_adds_axfeat_prefixed_features() {
        let root = tempdir().unwrap();
        let path = root.path().join(".build-target.toml");
        let request = request(path, "aarch64", "aarch64-unknown-none-softfloat");

        let build_info =
            load_build_info_with_makefile_features(&request, &[String::from("lockdep")]).unwrap();

        assert!(build_info.features.contains(&"qemu".to_string()));
        assert!(build_info.features.contains(&"ax-feat/lockdep".to_string()));
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
            plat_dyn: false,
        };
        let mut cargo = build_info.into_base_cargo_config_with_log(
            STARRY_PACKAGE.to_string(),
            request.target.clone(),
            vec![],
        );
        patch_starry_cargo_config(&mut cargo, &request).unwrap();

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
        let build_info = StarryBuildInfo::default_starry_for_target("x86_64-unknown-none");
        let mut cargo = build_info.into_base_cargo_config_with_log(
            "placeholder".to_string(),
            request.target.clone(),
            vec![],
        );

        patch_starry_cargo_config(&mut cargo, &request).unwrap();

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
                "ax-feat/bus-mmio".to_string(),
                "ax-feat/driver-sdmmc".to_string(),
                "ax-feat/plat-dyn".to_string(),
                "axplat-dyn/rk3588-clk".to_string(),
                "axplat-dyn/sdmmc".to_string(),
            ],
            log: LogLevel::Info,
            max_cpu_num: Some(8),
            plat_dyn: true,
        };
        let mut cargo = build_info.into_base_cargo_config_with_log(
            STARRY_PACKAGE.to_string(),
            request.target.clone(),
            StarryBuildInfo::build_cargo_args(&request.target, true),
        );

        patch_starry_cargo_config(&mut cargo, &request).unwrap();

        assert!(
            cargo
                .features
                .contains(&"axplat-dyn/rk3588-clk".to_string())
        );
        assert!(cargo.features.contains(&"axplat-dyn/sdmmc".to_string()));
        assert!(!cargo.features.contains(&"qemu".to_string()));
        assert!(!cargo.env.contains_key("AX_PLATFORM"));
        assert!(
            cargo
                .args
                .iter()
                .any(|arg| arg.contains("-Clink-arg=-Taxplat.x"))
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
                .join("starryos/.build-aarch64-unknown-none-softfloat.toml")
        );
    }

    #[test]
    fn patch_starry_cargo_config_keeps_linker_x_arg() {
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
        let build_info = StarryBuildInfo::default_starry_for_target(&request.target);
        let mut cargo = build_info.into_base_cargo_config_with_log(
            request.package.clone(),
            request.target.clone(),
            StarryBuildInfo::build_cargo_args(&request.target, false),
        );

        patch_starry_cargo_config(&mut cargo, &request).unwrap();

        assert!(
            cargo
                .args
                .iter()
                .any(|arg| arg.contains("-Clink-arg=-Tlinker.x"))
        );
    }

    #[test]
    fn ensure_starry_bin_arg_adds_bin_for_starryos_package() {
        let mut args = Vec::new();

        ensure_starry_bin_arg(&mut args, "starryos").unwrap();

        assert_eq!(args, vec!["--bin".to_string(), "starryos".to_string()]);
    }

    #[test]
    fn ensure_starry_bin_arg_keeps_existing_bin_arg() {
        let mut args = vec!["--bin".to_string(), "starryos".to_string()];

        ensure_starry_bin_arg(&mut args, STARRY_PACKAGE).unwrap();

        assert_eq!(args, vec!["--bin".to_string(), "starryos".to_string()]);
    }
}
