use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::Context;
use ostool::build::config::Cargo;

pub type StarryBuildInfo = crate::arceos::build::ArceosBuildInfo;
pub use crate::arceos::build::LogLevel;
use crate::context::{ResolvedStarryRequest, starry_arch_for_target_checked};

impl StarryBuildInfo {
    pub fn default_starry_for_target(target: &str) -> Self {
        let mut build_info = Self::default_for_target(target);
        build_info.plat_dyn = false;
        build_info.features = vec!["qemu".to_string()];
        build_info
    }
}

pub fn resolve_build_info_path(
    workspace_root: &Path,
    target: &str,
    explicit_path: Option<PathBuf>,
) -> anyhow::Result<PathBuf> {
    if let Some(path) = explicit_path {
        return Ok(path);
    }

    let _ = starry_arch_for_target_checked(target)?;
    Ok(crate::arceos::build::resolve_build_info_path_in_dir(
        &workspace_root.join("os/StarryOS/starryos"),
        target,
    ))
}

pub fn load_build_info(request: &ResolvedStarryRequest) -> anyhow::Result<StarryBuildInfo> {
    crate::arceos::build::load_or_create_build_info(&request.build_info_path, || {
        StarryBuildInfo::default_starry_for_target(&request.target)
    })
}

pub fn load_cargo_config(request: &ResolvedStarryRequest) -> anyhow::Result<Cargo> {
    to_cargo_config(load_build_info(request)?, request)
}

const ROOTFS_URL: &str = "https://github.com/Starry-OS/rootfs/releases/download/20260214";

pub fn rootfs_image_name(arch: &str) -> String {
    format!("rootfs-{arch}.img")
}

pub fn rootfs_artifact_dir(workspace_root: &Path, target: &str) -> PathBuf {
    workspace_root.join("target").join(target)
}

pub fn rootfs_disk_image_path(workspace_root: &Path, target: &str) -> PathBuf {
    rootfs_artifact_dir(workspace_root, target).join("disk.img")
}

pub fn default_qemu_args(disk_img: &Path) -> Vec<String> {
    vec![
        "-device".to_string(),
        "virtio-blk-pci,drive=disk0".to_string(),
        "-drive".to_string(),
        format!("id=disk0,if=none,format=raw,file={}", disk_img.display()),
        "-device".to_string(),
        "virtio-net-pci,netdev=net0".to_string(),
        "-netdev".to_string(),
        "user,id=net0,hostfwd=tcp::5555-:5555".to_string(),
    ]
}

pub fn ensure_rootfs_in_target_dir(
    workspace_root: &Path,
    arch: &str,
    target: &str,
) -> anyhow::Result<PathBuf> {
    let artifact_dir = rootfs_artifact_dir(workspace_root, target);
    let disk_img = artifact_dir.join("disk.img");
    let rootfs_name = rootfs_image_name(arch);
    let rootfs_img = artifact_dir.join(&rootfs_name);
    let rootfs_xz = artifact_dir.join(format!("{rootfs_name}.xz"));

    fs::create_dir_all(&artifact_dir)
        .with_context(|| format!("failed to create {}", artifact_dir.display()))?;

    if !rootfs_img.exists() {
        println!("image not found, downloading {}...", rootfs_name);
        let url = format!("{ROOTFS_URL}/{rootfs_name}.xz");
        let status = Command::new("curl")
            .arg("-f")
            .arg("-L")
            .arg(&url)
            .arg("-o")
            .arg(&rootfs_xz)
            .status()
            .with_context(|| format!("failed to spawn curl for {url}"))?;
        if !status.success() {
            anyhow::bail!("failed to download {}", url);
        }

        let status = Command::new("xz")
            .arg("-d")
            .arg("-f")
            .arg(&rootfs_xz)
            .status()
            .with_context(|| format!("failed to spawn xz for {}", rootfs_xz.display()))?;
        if !status.success() {
            anyhow::bail!("failed to decompress {}", rootfs_xz.display());
        }
    }

    fs::copy(&rootfs_img, &disk_img).with_context(|| {
        format!(
            "failed to copy {} to {}",
            rootfs_img.display(),
            disk_img.display()
        )
    })?;

    Ok(disk_img)
}

pub fn to_cargo_config(
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

    cargo.package = request.package.clone();
    cargo.target = request.target.clone();
    cargo.features.push("qemu".to_string());
    cargo.features.sort();
    cargo.features.dedup();

    cargo
        .env
        .insert("AX_ARCH".to_string(), request.arch.clone());
    cargo
        .env
        .insert("AX_TARGET".to_string(), request.target.clone());
    cargo
        .env
        .entry("AX_PLATFORM".to_string())
        .or_insert_with(|| platform.to_string());

    Ok(())
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

    fn request(path: PathBuf, arch: &str, target: &str) -> ResolvedStarryRequest {
        ResolvedStarryRequest {
            package: STARRY_PACKAGE.to_string(),
            arch: arch.to_string(),
            target: target.to_string(),
            plat_dyn: None,
            build_info_path: path,
            qemu_config: None,
            uboot_config: None,
        }
    }

    #[test]
    fn resolve_build_info_path_uses_default_starry_location() {
        let root = tempdir().unwrap();
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
        let written = fs::read_to_string(path).unwrap();
        assert!(written.contains("features = [\"qemu\"]"));
        assert!(written.contains("plat_dyn = true"));
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
        assert_eq!(cargo.features, vec!["net".to_string(), "qemu".to_string()]);
        assert_eq!(
            cargo.env.get("AX_ARCH").map(String::as_str),
            Some("aarch64")
        );
        assert_eq!(
            cargo.env.get("AX_TARGET").map(String::as_str),
            Some("aarch64-unknown-none-softfloat")
        );
        assert_eq!(
            cargo.env.get("AX_PLATFORM").map(String::as_str),
            Some("aarch64-qemu-virt")
        );
        assert_eq!(cargo.env.get("AX_LOG").map(String::as_str), Some("info"));
        assert_eq!(cargo.env.get("CUSTOM").map(String::as_str), Some("1"));
        assert!(cargo.to_bin);
    }

    #[test]
    fn load_cargo_config_uses_base_then_applies_starry_overrides() {
        let root = tempdir().unwrap();
        let path = root.path().join(".build-target.toml");
        fs::write(
            &path,
            r#"
log = "Info"
features = ["net"]

[env]
CUSTOM = "1"
"#,
        )
        .unwrap();

        let request = request(path, "aarch64", "aarch64-unknown-none-softfloat");
        let cargo = load_cargo_config(&request).unwrap();

        assert_eq!(cargo.package, STARRY_PACKAGE);
        assert_eq!(cargo.target, "aarch64-unknown-none-softfloat");
        assert_eq!(
            cargo.features,
            vec![
                "axfeat/defplat".to_string(),
                "net".to_string(),
                "qemu".to_string()
            ]
        );
        assert_eq!(
            cargo.env.get("AX_ARCH").map(String::as_str),
            Some("aarch64")
        );
        assert_eq!(
            cargo.env.get("AX_TARGET").map(String::as_str),
            Some("aarch64-unknown-none-softfloat")
        );
        assert_eq!(
            cargo.env.get("AX_PLATFORM").map(String::as_str),
            Some("aarch64-qemu-virt")
        );
        assert_eq!(cargo.env.get("AX_LOG").map(String::as_str), Some("info"));
        assert_eq!(cargo.env.get("CUSTOM").map(String::as_str), Some("1"));
        assert_eq!(
            cargo.args,
            vec![
                "--config".to_string(),
                "target.aarch64-unknown-none-softfloat.rustflags=[\"-Clink-arg=-Tlinker.x\",\"\
                 -Clink-arg=-no-pie\",\"-Clink-arg=-znostart-stop-gc\"]"
                    .to_string()
            ]
        );
        assert!(cargo.to_bin);
    }

    #[test]
    fn patch_starry_cargo_config_preserves_request_package() {
        let request = ResolvedStarryRequest {
            package: "starryos-test".to_string(),
            arch: "x86_64".to_string(),
            target: "x86_64-unknown-none".to_string(),
            plat_dyn: None,
            build_info_path: PathBuf::from("/tmp/.build.toml"),
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

        assert_eq!(cargo.package, "starryos-test");
    }

    #[test]
    fn load_cargo_config_honors_request_plat_dyn_override() {
        let root = tempdir().unwrap();
        let path = root.path().join(".build-target.toml");
        fs::write(
            &path,
            r#"
log = "Info"
plat_dyn = true
features = ["net"]

[env]
CUSTOM = "1"
"#,
        )
        .unwrap();

        let mut request = request(path, "aarch64", "aarch64-unknown-none-softfloat");
        request.plat_dyn = Some(false);
        let cargo = load_cargo_config(&request).unwrap();

        assert!(cargo.features.contains(&"axfeat/defplat".to_string()));
        assert!(!cargo.features.contains(&"axfeat/plat-dyn".to_string()));
        assert!(cargo.args.iter().any(|arg| arg.contains("-Tlinker.x")));
    }

    #[test]
    fn rootfs_disk_image_path_uses_workspace_target_triple_dir() {
        let root = Path::new("/tmp/workspace");
        let disk_img = rootfs_disk_image_path(root, "aarch64-unknown-none-softfloat");

        assert_eq!(
            disk_img,
            PathBuf::from("/tmp/workspace/target/aarch64-unknown-none-softfloat/disk.img")
        );
    }

    #[test]
    fn default_qemu_args_include_disk_and_network_defaults() {
        let args = default_qemu_args(Path::new("/tmp/disk.img"));

        assert_eq!(
            args,
            vec![
                "-device".to_string(),
                "virtio-blk-pci,drive=disk0".to_string(),
                "-drive".to_string(),
                "id=disk0,if=none,format=raw,file=/tmp/disk.img".to_string(),
                "-device".to_string(),
                "virtio-net-pci,netdev=net0".to_string(),
                "-netdev".to_string(),
                "user,id=net0,hostfwd=tcp::5555-:5555".to_string(),
            ]
        );
    }
}
