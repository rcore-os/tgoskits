use std::{
    fs,
    path::{Path, PathBuf},
};

use ostool::{
    build::{CargoQemuOverrideArgs, config::Cargo},
    run::qemu::QemuConfig,
};
use serde::{Deserialize, Serialize};

use crate::{
    arceos::build::ArceosBuildInfo,
    axvisor::board,
    context::{QemuRunConfig, ResolvedAxvisorRequest, arch_for_target_checked},
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

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub(crate) struct AxvisorBoardFile {
    pub(crate) target: String,
    #[serde(flatten)]
    pub(crate) config: AxvisorBoardConfig,
}

impl AxvisorBoardFile {
    pub(crate) fn into_board_config(self) -> AxvisorBoardConfig {
        self.config
    }

    fn into_loaded(self) -> LoadedAxvisorBuildConfig {
        let Self { target, config } = self;
        config.into_loaded(target)
    }
}

impl AxvisorBuildInfo {
    pub fn default_axvisor_for_target(target: &str) -> Self {
        let mut build_info = Self::default_for_target(target);
        build_info.features.clear();
        build_info
    }
}

impl AxvisorBoardConfig {
    fn into_loaded(self, target: String) -> LoadedAxvisorBuildConfig {
        LoadedAxvisorBuildConfig {
            build_info: self.arceos,
            target,
        }
    }
}

pub(crate) fn load_board_file(path: &Path) -> anyhow::Result<AxvisorBoardFile> {
    let content = fs::read_to_string(path).map_err(|e| {
        anyhow!(
            "failed to read Axvisor board config {}: {e}",
            path.display()
        )
    })?;
    toml::from_str(&content).map_err(|e| {
        anyhow!(
            "failed to parse Axvisor board config {}: {e}",
            path.display()
        )
    })
}

pub fn default_qemu_config_template_path(axvisor_dir: &Path, arch: &str) -> PathBuf {
    axvisor_dir.join(format!("scripts/ostool/qemu-{arch}.toml"))
}

pub fn resolve_build_info_path(
    axvisor_dir: &Path,
    target: &str,
    explicit_path: Option<PathBuf>,
) -> anyhow::Result<PathBuf> {
    if let Some(path) = explicit_path {
        return Ok(path);
    }

    let _ = arch_for_target_checked(target)?;
    Ok(crate::arceos::build::resolve_build_info_path_in_dir(
        axvisor_dir,
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

pub fn default_qemu_run_config(request: &ResolvedAxvisorRequest) -> anyhow::Result<QemuRunConfig> {
    let default_args = CargoQemuOverrideArgs {
        to_bin: Some(default_qemu_to_bin(&request.arch)?),
        args: Some(default_runtime_qemu_args(&request.arch, None)),
        ..Default::default()
    };

    let override_args = infer_rootfs_path(&request.vmconfigs)?.map_or_else(
        CargoQemuOverrideArgs::default,
        |rootfs_path| CargoQemuOverrideArgs {
            args: Some(default_runtime_qemu_args(&request.arch, Some(&rootfs_path))),
            ..Default::default()
        },
    );

    Ok(QemuRunConfig {
        qemu_config: None,
        default_args,
        override_args,
        ..Default::default()
    })
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

    normalize_axvisor_platform_features(&mut cargo.features);
    cargo.features.sort();
    cargo.features.dedup();
    Ok(())
}

fn normalize_axvisor_platform_features(features: &mut Vec<String>) {
    let has_axstd_defplat = features.iter().any(|feature| feature == "axstd/defplat");
    let has_axstd_myplat = features.iter().any(|feature| feature == "axstd/myplat");

    if has_axstd_defplat && !has_axstd_myplat {
        for feature in features.iter_mut() {
            if feature == "axstd/defplat" {
                *feature = "axstd/myplat".to_string();
            }
        }
    } else {
        features.retain(|feature| feature != "axstd/defplat");
    }
}

pub fn load_target_from_build_config(path: &Path) -> anyhow::Result<Option<String>> {
    let content = fs::read_to_string(path).map_err(|e| {
        anyhow!(
            "failed to read Axvisor build config {}: {e}",
            path.display()
        )
    })?;

    if let Ok(board_file) = toml::from_str::<AxvisorBoardFile>(&content) {
        return Ok(Some(board_file.target));
    }
    if toml::from_str::<AxvisorBuildInfo>(&content).is_ok() {
        return Ok(None);
    }

    Err(anyhow!("invalid Axvisor build config {}", path.display()))
}

pub fn prepare_default_qemu_config(request: &ResolvedAxvisorRequest) -> anyhow::Result<PathBuf> {
    let template_path = default_qemu_config_template_path(&request.axvisor_dir, &request.arch);
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

pub fn default_qemu_override_args(
    request: &ResolvedAxvisorRequest,
) -> anyhow::Result<CargoQemuOverrideArgs> {
    let template_path = default_qemu_config_template_path(&request.axvisor_dir, &request.arch);
    qemu_override_args_from_template(&template_path, request)
}

pub fn qemu_override_args_from_template(
    template_path: &Path,
    request: &ResolvedAxvisorRequest,
) -> anyhow::Result<CargoQemuOverrideArgs> {
    let mut config = load_qemu_config(template_path)?;
    if let Some(rootfs_path) = infer_rootfs_path(&request.vmconfigs)? {
        replace_rootfs_arg(&mut config.args, &rootfs_path);
    }

    Ok(CargoQemuOverrideArgs {
        args: Some(config.args),
        ..Default::default()
    })
}

fn default_qemu_to_bin(arch: &str) -> anyhow::Result<bool> {
    match arch {
        "aarch64" | "riscv64" | "loongarch64" => Ok(true),
        "x86_64" => Ok(false),
        _ => anyhow::bail!(
            "unsupported Axvisor architecture `{arch}`; expected one of aarch64, x86_64, riscv64, \
             loongarch64"
        ),
    }
}

fn default_runtime_qemu_args(arch: &str, rootfs_path: Option<&Path>) -> Vec<String> {
    let rootfs = rootfs_path
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "${workspaceFolder}/tmp/rootfs.img".to_string());

    match arch {
        "aarch64" => vec![
            "-nographic".to_string(),
            "-cpu".to_string(),
            "cortex-a72".to_string(),
            "-machine".to_string(),
            "virt,virtualization=on,gic-version=3".to_string(),
            "-smp".to_string(),
            "4".to_string(),
            "-device".to_string(),
            "virtio-blk-device,drive=disk0".to_string(),
            "-drive".to_string(),
            format!("id=disk0,if=none,format=raw,file={rootfs}"),
            "-append".to_string(),
            "root=/dev/vda rw init=/init".to_string(),
            "-m".to_string(),
            "8g".to_string(),
        ],
        "riscv64" => vec![
            "-nographic".to_string(),
            "-cpu".to_string(),
            "rv64".to_string(),
            "-machine".to_string(),
            "virt".to_string(),
            "-bios".to_string(),
            "default".to_string(),
            "-smp".to_string(),
            "4".to_string(),
            "-device".to_string(),
            "virtio-blk-device,drive=disk0".to_string(),
            "-drive".to_string(),
            format!("id=disk0,if=none,format=raw,file={rootfs}"),
            "-append".to_string(),
            "root=/dev/vda rw init=/init".to_string(),
            "-m".to_string(),
            "4g".to_string(),
        ],
        "x86_64" => vec![
            "-nographic".to_string(),
            "-cpu".to_string(),
            "host".to_string(),
            "-machine".to_string(),
            "q35".to_string(),
            "-smp".to_string(),
            "1".to_string(),
            "-accel".to_string(),
            "kvm".to_string(),
            "-device".to_string(),
            "virtio-blk-pci,drive=disk0".to_string(),
            "-drive".to_string(),
            format!("id=disk0,if=none,format=raw,file={rootfs}"),
            "-m".to_string(),
            "128M".to_string(),
        ],
        "loongarch64" => vec![
            "-nographic".to_string(),
            "-smp".to_string(),
            "4".to_string(),
            "-device".to_string(),
            "virtio-blk-device,drive=disk0".to_string(),
            "-drive".to_string(),
            format!("id=disk0,if=none,format=raw,file={rootfs}"),
            "-append".to_string(),
            "root=/dev/vda rw init=/init".to_string(),
            "-m".to_string(),
            "4g".to_string(),
        ],
        _ => vec![],
    }
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

fn load_qemu_config(path: &Path) -> anyhow::Result<QemuConfig> {
    let content = fs::read_to_string(path).map_err(|e| {
        anyhow!(
            "failed to read QEMU config template {}: {e}",
            path.display()
        )
    })?;
    toml::from_str(&content).map_err(|e| {
        anyhow!(
            "failed to parse QEMU config template {}: {e}",
            path.display()
        )
    })
}

fn replace_rootfs_arg(args: &mut Vec<String>, rootfs_path: &Path) {
    const DEFAULT_ROOTFS: &str = "${workspaceFolder}/tmp/rootfs.img";

    for arg in args {
        if arg.contains(DEFAULT_ROOTFS) {
            *arg = arg.replace(DEFAULT_ROOTFS, &rootfs_path.display().to_string());
        }
    }
}

fn load_build_config(request: &ResolvedAxvisorRequest) -> anyhow::Result<LoadedAxvisorBuildConfig> {
    println!("Using build config: {}", request.build_info_path.display());

    if !request.build_info_path.exists() {
        if let Some(parent) = request.build_info_path.parent() {
            fs::create_dir_all(parent)?;
        }
        if let Some(default_board) =
            board::default_board_for_target(&request.axvisor_dir, &request.target)?
        {
            fs::copy(&default_board.path, &request.build_info_path).map_err(|e| {
                anyhow!(
                    "failed to copy default board config {} to {}: {e}",
                    default_board.path.display(),
                    request.build_info_path.display()
                )
            })?;
            return Ok(default_board
                .config
                .into_loaded(default_board.target.clone()));
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

    if let Ok(board_config) = toml::from_str::<AxvisorBoardFile>(&content) {
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
    use std::{fs, path::Path};

    use tempfile::tempdir;

    use super::*;

    fn write_board(axvisor_dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = axvisor_dir
            .join("configs/board")
            .join(format!("{name}.toml"));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, body).unwrap();
        path
    }

    fn request(path: PathBuf, arch: &str, target: &str) -> ResolvedAxvisorRequest {
        ResolvedAxvisorRequest {
            package: AXVISOR_PACKAGE.to_string(),
            axvisor_dir: path
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from("os/axvisor")),
            arch: arch.to_string(),
            target: target.to_string(),
            plat_dyn: None,
            build_info_path: path,
            qemu_config: None,
            uboot_config: None,
            vmconfigs: vec![],
        }
    }

    #[test]
    fn resolve_build_info_path_uses_default_axvisor_location() {
        let root = tempdir().unwrap();
        let path = resolve_build_info_path(
            &root.path().join("os/axvisor"),
            "aarch64-unknown-none-softfloat",
            None,
        )
        .unwrap();

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
        let path = resolve_build_info_path(
            &root.path().join("os/axvisor"),
            "x86_64-unknown-none",
            Some(explicit.clone()),
        )
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
            resolve_build_info_path(&axvisor_dir, "aarch64-unknown-none-softfloat", None).unwrap();

        assert_eq!(path, bare);
    }

    #[test]
    fn load_build_info_writes_default_template_when_missing() {
        let root = tempdir().unwrap();
        let path = root
            .path()
            .join("os/axvisor/.build-aarch64-unknown-none-softfloat.toml");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        write_board(
            path.parent().unwrap(),
            "qemu-aarch64",
            r#"
env = { AX_IP = "10.0.2.15", AX_GW = "10.0.2.2" }
target = "aarch64-unknown-none-softfloat"
features = ["ept-level-4", "axstd/bus-mmio"]
log = "Info"
plat_dyn = true
vm_configs = []
"#,
        );

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
            axvisor_dir: root.path().join("os/axvisor"),
            arch: "aarch64".to_string(),
            target: "aarch64-unknown-none-softfloat".to_string(),
            plat_dyn: Some(true),
            build_info_path: config_path,
            qemu_config: None,
            uboot_config: None,
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
env = { AX_IP = "10.0.2.15", AX_GW = "10.0.2.2" }
features = []
log = "Info"
target = "aarch64-unknown-none-softfloat"
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
        let board_path = write_board(
            path.parent().unwrap(),
            "qemu-x86_64",
            r#"
env = { AX_IP = "10.0.2.15", AX_GW = "10.0.2.2" }
target = "x86_64-unknown-none"
features = ["ept-level-4", "fs"]
log = "Info"
vm_configs = []
"#,
        );

        let cargo = load_cargo_config(&ResolvedAxvisorRequest {
            package: AXVISOR_PACKAGE.to_string(),
            axvisor_dir: root.path().join("os/axvisor"),
            arch: "x86_64".to_string(),
            target: "x86_64-unknown-none".to_string(),
            plat_dyn: None,
            build_info_path: path.clone(),
            qemu_config: None,
            uboot_config: None,
            vmconfigs: vec![],
        })
        .unwrap();

        assert!(path.exists());
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            fs::read_to_string(board_path).unwrap()
        );
        assert!(cargo.features.contains(&"ept-level-4".to_string()));
        assert!(cargo.features.contains(&"fs".to_string()));
        assert!(!cargo.features.contains(&"axstd/plat-dyn".to_string()));
        assert!(!cargo.features.contains(&"axstd/defplat".to_string()));
        assert!(cargo.features.contains(&"axstd/myplat".to_string()));
    }

    #[test]
    fn load_cargo_config_replaces_axstd_defplat_with_myplat() {
        let root = tempdir().unwrap();
        let config_path = root.path().join(".build.toml");
        fs::write(
            &config_path,
            r#"
env = {}
features = ["axstd", "axstd/defplat", "ept-level-4"]
log = "Info"
plat_dyn = false
"#,
        )
        .unwrap();

        let cargo = load_cargo_config(&ResolvedAxvisorRequest {
            package: AXVISOR_PACKAGE.to_string(),
            axvisor_dir: root.path().join("os/axvisor"),
            arch: "aarch64".to_string(),
            target: "aarch64-unknown-none-softfloat".to_string(),
            plat_dyn: Some(false),
            build_info_path: config_path,
            qemu_config: None,
            uboot_config: None,
            vmconfigs: vec![],
        })
        .unwrap();

        assert!(!cargo.features.contains(&"axstd/defplat".to_string()));
        assert!(cargo.features.contains(&"axstd/myplat".to_string()));
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

    #[test]
    fn default_qemu_run_config_uses_ostool_default_path_resolution() {
        let run_config = default_qemu_run_config(&request(
            PathBuf::from("os/axvisor/.build-aarch64-unknown-none-softfloat.toml"),
            "aarch64",
            "aarch64-unknown-none-softfloat",
        ))
        .unwrap();

        assert!(run_config.qemu_config.is_none());
        assert_eq!(run_config.default_args.to_bin, Some(true));
        assert_eq!(
            run_config.default_args.args,
            Some(default_runtime_qemu_args("aarch64", None))
        );
        assert!(run_config.override_args.args.is_none());
    }

    #[test]
    fn default_qemu_run_config_overrides_rootfs_when_vmconfig_provides_one() {
        let root = tempdir().unwrap();
        let image_dir = root.path().join("image");
        fs::create_dir_all(&image_dir).unwrap();
        let rootfs_path = image_dir.join("rootfs.img");
        fs::write(&rootfs_path, b"rootfs").unwrap();
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

        let run_config = default_qemu_run_config(&ResolvedAxvisorRequest {
            package: AXVISOR_PACKAGE.to_string(),
            axvisor_dir: root.path().join("os/axvisor"),
            arch: "aarch64".to_string(),
            target: "aarch64-unknown-none-softfloat".to_string(),
            plat_dyn: None,
            build_info_path: root.path().join(".build.toml"),
            qemu_config: None,
            uboot_config: None,
            vmconfigs: vec![vmconfig],
        })
        .unwrap();

        assert_eq!(
            run_config.override_args.args,
            Some(default_runtime_qemu_args("aarch64", Some(&rootfs_path)))
        );
    }
}
