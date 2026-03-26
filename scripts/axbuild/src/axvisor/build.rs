use std::{
    fs,
    path::{Path, PathBuf},
};

use ostool::build::config::Cargo;
use serde::{Deserialize, Serialize};

use crate::{
    arceos::build::ArceosBuildInfo,
    axvisor::board,
    context::{ResolvedAxvisorRequest, arch_for_target_checked},
};

pub type AxvisorBuildInfo = crate::arceos::build::ArceosBuildInfo;
pub use crate::arceos::build::LogLevel;

pub const AXVISOR_PACKAGE: &str = "axvisor";

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Default)]
pub struct AxvisorBoardConfig {
    #[serde(flatten, default)]
    pub(crate) arceos: ArceosBuildInfo,
    #[serde(default)]
    pub vm_configs: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq)]
struct LoadedAxvisorBuildConfig {
    build_info: AxvisorBuildInfo,
    target: String,
}

impl AxvisorBuildInfo {
    pub fn default_axvisor_for_target(target: &str) -> Self {
        let mut build_info = Self::default_for_target(target);
        build_info.features.clear();
        build_info
    }
}

impl AxvisorBoardConfig {
    fn into_loaded(self) -> LoadedAxvisorBuildConfig {
        LoadedAxvisorBuildConfig {
            build_info: self.arceos,
            target: String::new(),
        }
    }
}

pub fn default_qemu_config_template_path(workspace_root: &Path, arch: &str) -> PathBuf {
    workspace_root.join(format!("os/axvisor/scripts/ostool/qemu-{arch}.toml"))
}

pub fn resolve_build_info_path(
    workspace_root: &Path,
    target: &str,
    explicit_path: Option<PathBuf>,
) -> anyhow::Result<PathBuf> {
    if let Some(path) = explicit_path {
        return Ok(path);
    }

    let _ = arch_for_target_checked(target)?;
    Ok(crate::arceos::build::resolve_build_info_path_in_dir(
        &workspace_root.join("os/axvisor"),
        target,
    ))
}

pub fn load_build_info(request: &ResolvedAxvisorRequest) -> anyhow::Result<AxvisorBuildInfo> {
    Ok(load_build_config(request)?.build_info)
}

pub fn load_cargo_config(request: &ResolvedAxvisorRequest) -> anyhow::Result<Cargo> {
    to_cargo_config(load_build_config(request)?, request)
}

pub fn default_qemu_args(arch: &str) -> anyhow::Result<Vec<String>> {
    let mut args = vec!["-m".to_string(), "2G".to_string()];
    match arch {
        "aarch64" => args.extend([
            "-machine".to_string(),
            "virt,virtualization=on,gic-version=3".to_string(),
        ]),
        "x86_64" => args.extend([
            "-accel".to_string(),
            "kvm".to_string(),
            "-cpu".to_string(),
            "host".to_string(),
        ]),
        "riscv64" | "loongarch64" => {}
        _ => anyhow::bail!(
            "unsupported Axvisor architecture `{arch}`; expected one of aarch64, x86_64, riscv64, \
             loongarch64"
        ),
    }
    Ok(args)
}

fn to_cargo_config(
    mut config: LoadedAxvisorBuildConfig,
    request: &ResolvedAxvisorRequest,
) -> anyhow::Result<Cargo> {
    config.target = request.target.clone();
    let mut cargo = config.build_info.into_prepared_base_cargo_config(
        &request.package,
        &config.target,
        request.plat_dyn,
    )?;
    patch_axvisor_cargo_config(&mut cargo, request)?;
    Ok(cargo)
}

fn patch_axvisor_cargo_config(
    cargo: &mut Cargo,
    request: &ResolvedAxvisorRequest,
) -> anyhow::Result<()> {
    cargo.package = request.package.clone();
    cargo.target = request.target.clone();
    cargo
        .env
        .insert("AX_ARCH".to_string(), request.arch.clone());
    cargo
        .env
        .insert("AX_TARGET".to_string(), request.target.clone());

    if !request.vmconfigs.is_empty() {
        let joined = std::env::join_paths(&request.vmconfigs)
            .map_err(|e| anyhow!("failed to join vmconfig paths: {e}"))?;
        cargo.env.insert(
            "AXVISOR_VM_CONFIGS".to_string(),
            joined.to_string_lossy().into_owned(),
        );
    }

    cargo.features.sort();
    cargo.features.dedup();
    Ok(())
}

pub fn load_target_from_build_config(path: &Path) -> anyhow::Result<Option<String>> {
    let content = fs::read_to_string(path).map_err(|e| {
        anyhow!(
            "failed to read Axvisor build config {}: {e}",
            path.display()
        )
    })?;

    let value: toml::Value = toml::from_str(&content).map_err(|e| {
        anyhow!(
            "failed to parse Axvisor build config {}: {e}",
            path.display()
        )
    })?;
    if let Some(target) = value
        .get("target")
        .and_then(|value| value.as_str())
        .map(ToOwned::to_owned)
    {
        return Ok(Some(target));
    }
    if toml::from_str::<AxvisorBuildInfo>(&content).is_ok() {
        return Ok(None);
    }

    Err(anyhow!("invalid Axvisor build config {}", path.display()))
}

pub fn prepare_default_qemu_config(
    workspace_root: &Path,
    request: &ResolvedAxvisorRequest,
) -> anyhow::Result<PathBuf> {
    let template_path = default_qemu_config_template_path(workspace_root, &request.arch);
    prepare_qemu_config_from_template(&template_path, request)
}

pub fn prepare_qemu_config_from_template(
    template_path: &Path,
    request: &ResolvedAxvisorRequest,
) -> anyhow::Result<PathBuf> {
    let mut content = fs::read_to_string(template_path).map_err(|e| {
        anyhow!(
            "failed to read QEMU config template {}: {e}",
            template_path.display()
        )
    })?;

    if let Some(rootfs_path) = infer_rootfs_path(&request.vmconfigs)? {
        content = content.replace(r#"# "-drive","#, r#""-drive","#);
        content = content.replace(
            r#"# "id=disk0,if=none,format=raw,file=${workspaceFolder}/tmp/rootfs.img","#,
            r#""id=disk0,if=none,format=raw,file=${workspaceFolder}/tmp/rootfs.img","#,
        );
        content = content.replace(
            "${workspaceFolder}/tmp/rootfs.img",
            &rootfs_path.display().to_string(),
        );
    }

    let output_path = std::env::temp_dir().join(format!("axvisor-qemu-{}.toml", request.arch));
    fs::write(&output_path, content).map_err(|e| {
        anyhow!(
            "failed to write generated QEMU config {}: {e}",
            output_path.display()
        )
    })?;
    Ok(output_path)
}

fn infer_rootfs_path(vmconfigs: &[PathBuf]) -> anyhow::Result<Option<PathBuf>> {
    for vmconfig in vmconfigs {
        let content = fs::read_to_string(vmconfig)
            .map_err(|e| anyhow!("failed to read vm config {}: {e}", vmconfig.display()))?;
        let value: toml::Value = toml::from_str(&content)
            .map_err(|e| anyhow!("failed to parse vm config {}: {e}", vmconfig.display()))?;
        let Some(kernel_path) = value
            .get("kernel")
            .and_then(|kernel| kernel.get("kernel_path"))
            .and_then(|path| path.as_str())
        else {
            continue;
        };
        let rootfs_path = Path::new(kernel_path)
            .parent()
            .map(|dir| dir.join("rootfs.img"));
        if let Some(rootfs_path) = rootfs_path
            && rootfs_path.exists()
        {
            return Ok(Some(rootfs_path));
        }
    }
    Ok(None)
}

fn load_build_config(request: &ResolvedAxvisorRequest) -> anyhow::Result<LoadedAxvisorBuildConfig> {
    println!("Using build config: {}", request.build_info_path.display());

    if !request.build_info_path.exists() {
        if let Some(parent) = request.build_info_path.parent() {
            fs::create_dir_all(parent)?;
        }
        if let Some(default_board) = board::default_board_for_target(&request.target) {
            fs::write(
                &request.build_info_path,
                toml::to_string_pretty(&default_board)?,
            )?;
            return Ok(default_board.into_loaded());
        }

        let default_build_info = AxvisorBuildInfo::default_axvisor_for_target(&request.target);
        fs::write(
            &request.build_info_path,
            toml::to_string_pretty(&default_build_info)?,
        )?;

        return Ok(LoadedAxvisorBuildConfig {
            build_info: default_build_info,
            target: request.target.clone(),
        });
    }

    let content = fs::read_to_string(&request.build_info_path).map_err(|e| {
        anyhow!(
            "failed to read Axvisor build config {}: {e}",
            request.build_info_path.display()
        )
    })?;

    if let Ok(board_config) = toml::from_str::<AxvisorBoardConfig>(&content) {
        return Ok(board_config.into_loaded());
    }

    if request.build_info_path.exists() {
        return toml::from_str::<AxvisorBuildInfo>(&content)
            .map(|build_info| LoadedAxvisorBuildConfig {
                build_info,
                target: request.target.clone(),
            })
            .map_err(|e| {
                anyhow!(
                    "failed to parse build info {}: {e}",
                    request.build_info_path.display()
                )
            });
    }

    Err(anyhow!(
        "failed to parse build info {}",
        request.build_info_path.display()
    ))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    fn request(path: PathBuf, arch: &str, target: &str) -> ResolvedAxvisorRequest {
        ResolvedAxvisorRequest {
            package: AXVISOR_PACKAGE.to_string(),
            arch: arch.to_string(),
            target: target.to_string(),
            plat_dyn: None,
            build_info_path: path,
            qemu_config: None,
            vmconfigs: vec![],
        }
    }

    #[test]
    fn resolve_build_info_path_uses_default_axvisor_location() {
        let root = tempdir().unwrap();
        let path =
            resolve_build_info_path(root.path(), "aarch64-unknown-none-softfloat", None).unwrap();

        assert_eq!(
            path,
            root.path()
                .join("os/axvisor/.build-aarch64-unknown-none-softfloat.toml")
        );
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
    fn resolve_build_info_path_prefers_existing_bare_name() {
        let root = tempdir().unwrap();
        let axvisor_dir = root.path().join("os/axvisor");
        fs::create_dir_all(&axvisor_dir).unwrap();
        let bare = axvisor_dir.join("build-aarch64-unknown-none-softfloat.toml");
        let dotted = axvisor_dir.join(".build-aarch64-unknown-none-softfloat.toml");
        fs::write(&bare, "").unwrap();
        fs::write(&dotted, "").unwrap();

        let path =
            resolve_build_info_path(root.path(), "aarch64-unknown-none-softfloat", None).unwrap();

        assert_eq!(path, bare);
    }

    #[test]
    fn load_build_info_writes_default_template_when_missing() {
        let root = tempdir().unwrap();
        let path = root
            .path()
            .join("os/axvisor/.build-aarch64-unknown-none-softfloat.toml");
        fs::create_dir_all(path.parent().unwrap()).unwrap();

        let build_info = load_build_info(&request(
            path.clone(),
            "aarch64",
            "aarch64-unknown-none-softfloat",
        ))
        .unwrap();

        assert!(build_info.plat_dyn);
        assert!(build_info.features.contains(&"ept-level-4".to_string()));
        assert!(build_info.features.contains(&"axstd/bus-mmio".to_string()));
        assert!(path.exists());
    }

    #[test]
    fn load_cargo_config_injects_vmconfigs() {
        let root = tempdir().unwrap();
        let config_path = root.path().join(".build.toml");
        fs::write(
            &config_path,
            r#"
env = {}
features = ["fs", "ept-level-4"]
log = "Info"
plat_dyn = true
"#,
        )
        .unwrap();

        let cargo = load_cargo_config(&ResolvedAxvisorRequest {
            package: AXVISOR_PACKAGE.to_string(),
            arch: "aarch64".to_string(),
            target: "aarch64-unknown-none-softfloat".to_string(),
            plat_dyn: Some(true),
            build_info_path: config_path,
            qemu_config: None,
            vmconfigs: vec![root.path().join("a.toml"), root.path().join("b.toml")],
        })
        .unwrap();

        assert_eq!(cargo.package, AXVISOR_PACKAGE);
        assert_eq!(cargo.target, "aarch64-unknown-none-softfloat");
        assert_eq!(
            cargo.env.get("AX_ARCH").map(String::as_str),
            Some("aarch64")
        );
        assert_eq!(
            cargo.env.get("AX_TARGET").map(String::as_str),
            Some("aarch64-unknown-none-softfloat")
        );
        assert!(cargo.env.contains_key("AXVISOR_VM_CONFIGS"));
    }

    #[test]
    fn load_target_from_board_config_reads_target() {
        let root = tempdir().unwrap();
        let path = root.path().join("qemu-aarch64.toml");
        fs::write(
            &path,
            r#"
cargo_args = []
features = []
log = "Info"
target = "aarch64-unknown-none-softfloat"
to_bin = true
vm_configs = []
"#,
        )
        .unwrap();

        assert_eq!(
            load_target_from_build_config(&path).unwrap(),
            Some("aarch64-unknown-none-softfloat".to_string())
        );
    }

    #[test]
    fn load_cargo_config_uses_board_defaults_when_default_file_is_missing() {
        let root = tempdir().unwrap();
        let path = root
            .path()
            .join("os/axvisor/.build-x86_64-unknown-none.toml");
        fs::create_dir_all(path.parent().unwrap()).unwrap();

        let cargo = load_cargo_config(&ResolvedAxvisorRequest {
            package: AXVISOR_PACKAGE.to_string(),
            arch: "x86_64".to_string(),
            target: "x86_64-unknown-none".to_string(),
            plat_dyn: None,
            build_info_path: path.clone(),
            qemu_config: None,
            vmconfigs: vec![],
        })
        .unwrap();

        assert!(path.exists());
        assert!(cargo.features.contains(&"ept-level-4".to_string()));
        assert!(cargo.features.contains(&"fs".to_string()));
        assert!(!cargo.features.contains(&"axstd/plat-dyn".to_string()));
    }

    #[test]
    fn default_qemu_args_enable_virtualization_support() {
        assert_eq!(
            default_qemu_args("aarch64").unwrap(),
            vec![
                "-m".to_string(),
                "2G".to_string(),
                "-machine".to_string(),
                "virt,virtualization=on,gic-version=3".to_string()
            ]
        );
        assert_eq!(
            default_qemu_args("x86_64").unwrap(),
            vec![
                "-m".to_string(),
                "2G".to_string(),
                "-accel".to_string(),
                "kvm".to_string(),
                "-cpu".to_string(),
                "host".to_string()
            ]
        );
    }

    #[test]
    fn infer_rootfs_path_uses_vmconfig_kernel_sibling() {
        let root = tempdir().unwrap();
        let image_dir = root.path().join("image");
        fs::create_dir_all(&image_dir).unwrap();
        fs::write(image_dir.join("rootfs.img"), b"rootfs").unwrap();
        let vmconfig = root.path().join("vm.toml");
        fs::write(
            &vmconfig,
            format!(
                r#"
[kernel]
kernel_path = "{}"
"#,
                image_dir.join("qemu-aarch64").display()
            ),
        )
        .unwrap();

        assert_eq!(
            infer_rootfs_path(&[vmconfig]).unwrap(),
            Some(image_dir.join("rootfs.img"))
        );
    }
}
