use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, anyhow};
use ostool::build::config::Cargo;
use serde::{Deserialize, Serialize};

use crate::{
    axvisor::board,
    build::BuildInfo,
    context::{ResolvedAxvisorRequest, arch_for_target_checked},
};

mod x86;

pub type AxvisorBuildInfo = crate::build::BuildInfo;
pub use crate::build::LogLevel;

pub const AXVISOR_PACKAGE: &str = "axvisor";

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Default)]
pub struct AxvisorBoardConfig {
    #[serde(flatten, default)]
    pub(crate) build_info: BuildInfo,
    #[serde(default)]
    pub vm_configs: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq)]
struct LoadedAxvisorBuildConfig {
    build_info: AxvisorBuildInfo,
    target: String,
    vm_configs: Vec<PathBuf>,
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

pub(crate) fn default_axvisor_build_info_for_target(target: &str) -> AxvisorBuildInfo {
    let mut build_info = AxvisorBuildInfo::default_for_target(target);
    build_info.features.clear();
    build_info
}

impl AxvisorBoardConfig {
    fn into_loaded(self, target: String) -> LoadedAxvisorBuildConfig {
        LoadedAxvisorBuildConfig {
            build_info: self.build_info,
            target,
            vm_configs: self.vm_configs,
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

pub(crate) fn resolve_build_info_path(
    axvisor_dir: &Path,
    target: &str,
    explicit_path: Option<PathBuf>,
) -> anyhow::Result<PathBuf> {
    if let Some(path) = explicit_path {
        return Ok(path);
    }

    let _ = arch_for_target_checked(target)?;
    Ok(default_build_info_path(axvisor_dir, target))
}

pub(crate) fn workspace_root_from_axvisor_dir(axvisor_dir: &Path) -> PathBuf {
    axvisor_dir
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| axvisor_dir.to_path_buf())
}

pub(crate) fn default_build_info_path(axvisor_dir: &Path, target: &str) -> PathBuf {
    crate::build::default_build_info_path_in_workspace(
        &workspace_root_from_axvisor_dir(axvisor_dir),
        AXVISOR_PACKAGE,
        target,
    )
}

pub(crate) fn load_cargo_config(request: &ResolvedAxvisorRequest) -> anyhow::Result<Cargo> {
    let metadata =
        crate::build::cached_workspace_metadata().context("failed to load workspace metadata")?;
    to_cargo_config(load_build_config(request)?, request, metadata)
}

fn to_cargo_config(
    mut config: LoadedAxvisorBuildConfig,
    request: &ResolvedAxvisorRequest,
    metadata: &cargo_metadata::Metadata,
) -> anyhow::Result<Cargo> {
    config.target = request.target.clone();
    let known_platforms = platform_feature_names(metadata);
    reject_unsupported_nested_platform_features(&config.build_info.features, &known_platforms)?;
    let plat_dyn = config
        .build_info
        .effective_plat_dyn(&config.target, request.plat_dyn);
    normalize_axvisor_feature_surface(
        &mut config.build_info.features,
        &config.target,
        plat_dyn,
        metadata,
    )?;
    let mut cargo = config
        .build_info
        .into_prepared_base_cargo_config_with_metadata(
            &request.package,
            &config.target,
            request.plat_dyn,
            metadata,
        )?;
    if plat_dyn {
        cargo.features.retain(|feature| {
            !matches!(
                feature.as_str(),
                "ax-std/plat-dyn" | "ax-feat/plat-dyn" | "dyn-plat"
            )
        });
        cargo.features.push("dyn-plat".to_string());
    }
    patch_axvisor_cargo_config(&mut cargo, request, metadata, &config.vm_configs)?;
    Ok(cargo)
}

fn patch_axvisor_cargo_config(
    cargo: &mut Cargo,
    request: &ResolvedAxvisorRequest,
    metadata: &cargo_metadata::Metadata,
    config_vmconfigs: &[PathBuf],
) -> anyhow::Result<()> {
    cargo.package = request.package.clone();
    cargo.to_bin = default_axvisor_to_bin(&request.arch);
    ensure_axvisor_bin_arg(&mut cargo.args);
    cargo
        .env
        .insert("AX_ARCH".to_string(), request.arch.clone());
    cargo
        .env
        .insert("AX_TARGET".to_string(), request.target.clone());
    let plat_dyn = cargo.features.iter().any(|feature| feature == "dyn-plat");
    normalize_axvisor_feature_surface(&mut cargo.features, &request.target, plat_dyn, metadata)?;

    let vmconfigs = if request.vmconfigs.is_empty() {
        config_vmconfigs
            .iter()
            .map(|path| resolve_build_config_vmconfig_path(request, path))
            .collect::<Vec<_>>()
    } else {
        request.vmconfigs.clone()
    };
    if !vmconfigs.is_empty() {
        let joined = std::env::join_paths(&vmconfigs)
            .map_err(|e| anyhow!("failed to join vmconfig paths: {e}"))?;
        cargo.env.insert(
            "AXVISOR_VM_CONFIGS".to_string(),
            joined.to_string_lossy().into_owned(),
        );
    }

    if request.arch == "x86_64" {
        x86::normalize_backend_features(&mut cargo.features)?;
    }
    cargo.features.sort();
    cargo.features.dedup();
    Ok(())
}

fn normalize_axvisor_feature_surface(
    features: &mut Vec<String>,
    target: &str,
    plat_dyn: bool,
    metadata: &cargo_metadata::Metadata,
) -> anyhow::Result<()> {
    let known_platforms = platform_feature_names(metadata);
    let axvisor_platforms = axvisor_platform_features(metadata)?;
    let selected_platform =
        select_axvisor_platform_feature(features, target, plat_dyn, &axvisor_platforms)?;

    let Some(platform) = selected_platform else {
        return Ok(());
    };

    features.retain(|feature| {
        if feature == &platform {
            return true;
        }

        if axvisor_platforms
            .iter()
            .any(|platform| platform.feature == *feature)
        {
            return false;
        }

        if nested_platform_feature_name(feature, &known_platforms).is_some() {
            return false;
        }

        true
    });
    if !features.iter().any(|feature| feature == &platform) {
        features.push(platform);
    }
    Ok(())
}

fn reject_unsupported_nested_platform_features(
    features: &[String],
    known_platforms: &[String],
) -> anyhow::Result<()> {
    if let Some(feature) = features
        .iter()
        .find(|feature| nested_platform_feature_name(feature, known_platforms).is_some())
    {
        return Err(anyhow!(
            "Axvisor build configs must use Axvisor top-level platform features; found `{feature}`"
        ));
    }
    Ok(())
}

fn axvisor_platform_features(
    metadata: &cargo_metadata::Metadata,
) -> anyhow::Result<Vec<AxvisorPlatformFeature>> {
    let mut features = Vec::new();
    for platform in platform_metadata_entries(metadata) {
        let feature = platform.platform.clone();
        if axvisor_declares_feature(metadata, &feature)? {
            features.push(AxvisorPlatformFeature {
                feature,
                arch: Some(platform.arch),
                default_for_arch: platform.default_for_arch,
                dynamic: platform.dynamic,
            });
        }
    }
    if axvisor_declares_feature(metadata, "dyn-plat")? {
        features.push(AxvisorPlatformFeature {
            feature: "dyn-plat".to_string(),
            arch: None,
            default_for_arch: false,
            dynamic: true,
        });
    }
    features.sort_by(|a, b| a.feature.cmp(&b.feature));
    features.dedup_by(|a, b| a.feature == b.feature);
    Ok(features)
}

fn select_axvisor_platform_feature(
    features: &[String],
    target: &str,
    plat_dyn: bool,
    axvisor_platforms: &[AxvisorPlatformFeature],
) -> anyhow::Result<Option<String>> {
    let explicit = features
        .iter()
        .filter(|feature| {
            axvisor_platforms
                .iter()
                .any(|platform| platform.feature == ***feature)
        })
        .cloned()
        .collect::<Vec<_>>();

    if explicit.len() > 1 {
        return Err(anyhow!(
            "Axvisor build configs must select only one platform feature; found {}",
            explicit.join(", ")
        ));
    }
    if let Some(platform) = explicit.into_iter().next() {
        return Ok(Some(platform));
    }

    default_axvisor_platform_feature(target, plat_dyn, axvisor_platforms)
}

fn default_axvisor_platform_feature(
    target: &str,
    plat_dyn: bool,
    axvisor_platforms: &[AxvisorPlatformFeature],
) -> anyhow::Result<Option<String>> {
    if plat_dyn {
        return Ok(axvisor_platforms
            .iter()
            .find(|platform| platform.feature == "dyn-plat")
            .map(|platform| platform.feature.clone()));
    }

    let arch = arch_for_target_checked(target)?;
    let candidates = axvisor_platforms
        .iter()
        .filter(|platform| {
            !platform.dynamic
                && platform
                    .arch
                    .as_deref()
                    .is_some_and(|platform_arch| platform_arch == arch)
        })
        .collect::<Vec<_>>();
    let defaults = candidates
        .iter()
        .copied()
        .filter(|platform| platform.default_for_arch)
        .collect::<Vec<_>>();
    let platform = match defaults.as_slice() {
        [platform] => Some(*platform),
        [] => match candidates.as_slice() {
            [platform] => Some(*platform),
            [] => None,
            _ => {
                return Err(anyhow!(
                    "Axvisor build configs must select an explicit platform feature for arch \
                     `{arch}`"
                ));
            }
        },
        _ => {
            return Err(anyhow!(
                "multiple Axvisor default platform features are registered for arch `{arch}`"
            ));
        }
    };
    Ok(platform.map(|platform| platform.feature.clone()))
}

fn platform_feature_names(metadata: &cargo_metadata::Metadata) -> Vec<String> {
    let mut platforms = platform_metadata_entries(metadata)
        .into_iter()
        .map(|platform| platform.platform)
        .collect::<Vec<_>>();
    platforms.sort();
    platforms.dedup();
    platforms
}

fn platform_metadata_entries(metadata: &cargo_metadata::Metadata) -> Vec<AxplatMetadata> {
    metadata
        .packages
        .iter()
        .filter_map(|package| {
            package
                .metadata
                .get("axplat")
                .cloned()
                .and_then(|metadata| serde_json::from_value::<AxplatMetadata>(metadata).ok())
        })
        .collect()
}

fn axvisor_declares_feature(
    metadata: &cargo_metadata::Metadata,
    feature: &str,
) -> anyhow::Result<bool> {
    Ok(metadata
        .packages
        .iter()
        .find(|package| {
            metadata.workspace_members.contains(&package.id) && package.name == AXVISOR_PACKAGE
        })
        .ok_or_else(|| anyhow!("workspace package `{AXVISOR_PACKAGE}` not found"))?
        .features
        .contains_key(feature))
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
#[serde(rename_all = "kebab-case")]
struct AxplatMetadata {
    platform: String,
    arch: String,
    default_for_arch: bool,
    dynamic: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AxvisorPlatformFeature {
    feature: String,
    arch: Option<String>,
    default_for_arch: bool,
    dynamic: bool,
}

fn nested_platform_feature_name<'a>(
    feature: &'a str,
    known_platforms: &[String],
) -> Option<&'a str> {
    feature
        .strip_prefix("ax-std/")
        .or_else(|| feature.strip_prefix("ax-feat/"))
        .or_else(|| feature.strip_prefix("ax-hal/"))
        .filter(|name| is_platform_control_feature(name, known_platforms))
}

fn is_platform_control_feature(name: &str, known_platforms: &[String]) -> bool {
    matches!(name, "plat-dyn" | "defplat" | "myplat")
        || known_platforms.iter().any(|platform| platform == name)
}

fn resolve_build_config_vmconfig_path(request: &ResolvedAxvisorRequest, path: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    let workspace_root = request
        .axvisor_dir
        .parent()
        .and_then(Path::parent)
        .unwrap_or(&request.axvisor_dir);
    workspace_root.join(path)
}

fn default_axvisor_to_bin(arch: &str) -> bool {
    !matches!(arch, "x86_64" | "loongarch64")
}

fn ensure_axvisor_bin_arg(args: &mut Vec<String>) {
    if args.iter().any(|arg| arg == "--bin") {
        return;
    }

    args.push("--bin".to_string());
    args.push(AXVISOR_PACKAGE.to_string());
}

pub(crate) fn load_target_from_build_config(path: &Path) -> anyhow::Result<Option<String>> {
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
            let mut loaded = default_board
                .config
                .into_loaded(default_board.target.clone());
            let content = fs::read_to_string(&default_board.path).map_err(|e| {
                anyhow!(
                    "failed to read Axvisor default board config {}: {e}",
                    default_board.path.display()
                )
            })?;
            crate::build::apply_target_defaults_if_plat_dyn_unspecified(
                &mut loaded.build_info,
                &loaded.target,
                &content,
            );
            if let Some(smp) = request.smp {
                loaded.build_info.max_cpu_num = Some(smp);
            }
            return Ok(loaded);
        }

        let default_build_info = default_axvisor_build_info_for_target(&request.target);
        fs::write(
            &request.build_info_path,
            toml::to_string_pretty(&default_build_info)?,
        )?;

        let mut loaded = LoadedAxvisorBuildConfig {
            build_info: default_build_info,
            target: request.target.clone(),
            vm_configs: Vec::new(),
        };
        if let Some(smp) = request.smp {
            loaded.build_info.max_cpu_num = Some(smp);
        }
        return Ok(loaded);
    }

    let content = fs::read_to_string(&request.build_info_path).map_err(|e| {
        anyhow!(
            "failed to read Axvisor build config {}: {e}",
            request.build_info_path.display()
        )
    })?;

    if let Ok(board_config) = toml::from_str::<AxvisorBoardFile>(&content) {
        let mut loaded = board_config.into_loaded();
        crate::build::apply_target_defaults_if_plat_dyn_unspecified(
            &mut loaded.build_info,
            &loaded.target,
            &content,
        );
        if let Some(smp) = request.smp {
            loaded.build_info.max_cpu_num = Some(smp);
        }
        return Ok(loaded);
    }

    toml::from_str::<AxvisorBuildInfo>(&content)
        .map(|mut build_info| {
            crate::build::apply_target_defaults_if_plat_dyn_unspecified(
                &mut build_info,
                &request.target,
                &content,
            );
            let mut loaded = LoadedAxvisorBuildConfig {
                build_info,
                target: request.target.clone(),
                vm_configs: Vec::new(),
            };
            if let Some(smp) = request.smp {
                loaded.build_info.max_cpu_num = Some(smp);
            }
            loaded
        })
        .map_err(|e| {
            anyhow!(
                "failed to parse build info {}: {e}",
                request.build_info_path.display()
            )
        })
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
            smp: None,
            debug: false,
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
                .join("tmp/axbuild/config/axvisor/build-aarch64-unknown-none-softfloat.toml")
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
    fn resolve_build_info_path_ignores_source_tree_defaults() {
        let root = tempdir().unwrap();
        let axvisor_dir = root.path().join("os/axvisor");
        fs::create_dir_all(&axvisor_dir).unwrap();
        let bare = axvisor_dir.join("build-aarch64-unknown-none-softfloat.toml");
        let dotted = axvisor_dir.join(".build-aarch64-unknown-none-softfloat.toml");
        fs::write(&bare, "").unwrap();
        fs::write(&dotted, "").unwrap();

        let path =
            resolve_build_info_path(&axvisor_dir, "aarch64-unknown-none-softfloat", None).unwrap();

        assert_eq!(
            path,
            root.path()
                .join("tmp/axbuild/config/axvisor/build-aarch64-unknown-none-softfloat.toml")
        );
    }

    #[test]
    fn load_cargo_config_writes_default_template_when_missing() {
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
features = ["ept-level-4"]
log = "Info"
plat_dyn = true
vm_configs = []
"#,
        );

        let cargo = load_cargo_config(&request(
            path.clone(),
            "aarch64",
            "aarch64-unknown-none-softfloat",
        ))
        .unwrap();

        assert!(cargo.features.contains(&"ept-level-4".to_string()));
        assert!(cargo.features.contains(&"dyn-plat".to_string()));
        assert!(
            !cargo
                .features
                .contains(&concat!("ax-driver/", "plat-dyn").to_string())
        );
        assert!(
            !cargo
                .features
                .contains(&concat!("ax-std/", "plat-dyn").to_string())
        );
        assert!(path.exists());
    }

    #[test]
    fn load_cargo_config_injects_vmconfigs() {
        let root = tempdir().unwrap();
        let config_path = root.path().join(".build.toml");
        let vmconfigs = vec![root.path().join("a.toml"), root.path().join("b.toml")];
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
            smp: None,
            debug: false,
            build_info_path: config_path,
            qemu_config: None,
            uboot_config: None,
            vmconfigs: vmconfigs.clone(),
        })
        .unwrap();

        assert_eq!(cargo.package, AXVISOR_PACKAGE);
        assert!(
            cargo
                .target
                .ends_with("scripts/targets/pie/aarch64-unknown-none-softfloat.json")
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
            cargo.env.get("AXVISOR_VM_CONFIGS").map(String::as_str),
            Some(
                std::env::join_paths(&vmconfigs)
                    .unwrap()
                    .to_string_lossy()
                    .as_ref()
            )
        );
        assert_eq!(
            cargo
                .args
                .windows(2)
                .find_map(|window| (window[0] == "--bin").then_some(window[1].as_str())),
            Some("axvisor")
        );
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
features = ["x86-qemu-q35", "ept-level-4", "fs", "vmx"]
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
            smp: None,
            debug: false,
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
        assert!(cargo.features.contains(&"vmx".to_string()));
        assert!(!cargo.features.contains(&"ax-std/plat-dyn".to_string()));
        assert!(!cargo.features.contains(&"ax-std/defplat".to_string()));
        assert!(!cargo.features.contains(&"ax-std/x86-pc".to_string()));
        assert!(!cargo.features.contains(&"ax-hal/x86-pc".to_string()));
        assert!(cargo.features.contains(&"x86-qemu-q35".to_string()));
    }

    #[test]
    fn load_cargo_config_rejects_non_dynamic_aarch64_without_custom_platform() {
        let root = tempdir().unwrap();
        let config_path = root.path().join(".build.toml");
        fs::write(
            &config_path,
            r#"
env = {}
features = ["ax-std", "ept-level-4"]
log = "Info"
plat_dyn = false
"#,
        )
        .unwrap();

        let result = load_cargo_config(&ResolvedAxvisorRequest {
            package: AXVISOR_PACKAGE.to_string(),
            axvisor_dir: root.path().join("os/axvisor"),
            arch: "aarch64".to_string(),
            target: "aarch64-unknown-none-softfloat".to_string(),
            plat_dyn: Some(false),
            smp: None,
            debug: false,
            build_info_path: config_path,
            qemu_config: None,
            uboot_config: None,
            vmconfigs: vec![],
        });

        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("no default platform package is registered for arch `aarch64`")
        );
    }

    #[test]
    fn load_cargo_config_rejects_nested_axstd_platform_feature() {
        let root = tempdir().unwrap();
        let config_path = root.path().join(".build.toml");
        fs::write(
            &config_path,
            r#"
env = {}
features = ["ax-std/x86-qemu-q35", "ept-level-4"]
log = "Info"
plat_dyn = false
"#,
        )
        .unwrap();

        let err = load_cargo_config(&ResolvedAxvisorRequest {
            package: AXVISOR_PACKAGE.to_string(),
            axvisor_dir: root.path().join("os/axvisor"),
            arch: "x86_64".to_string(),
            target: "x86_64-unknown-none".to_string(),
            plat_dyn: Some(false),
            smp: None,
            debug: false,
            build_info_path: config_path,
            qemu_config: None,
            uboot_config: None,
            vmconfigs: vec![],
        })
        .unwrap_err();

        assert!(
            err.to_string()
                .contains("Axvisor top-level platform features")
        );
    }

    #[test]
    fn load_cargo_config_rejects_low_level_platform_feature() {
        let root = tempdir().unwrap();
        let config_path = root.path().join(".build.toml");
        let low_level_platform_feature = ["ax-hal", "x86-pc"].join("/");
        fs::write(
            &config_path,
            format!(
                r#"
env = {{}}
features = ["{low_level_platform_feature}", "ept-level-4"]
log = "Info"
plat_dyn = false
"#,
            ),
        )
        .unwrap();

        let err = load_cargo_config(&ResolvedAxvisorRequest {
            package: AXVISOR_PACKAGE.to_string(),
            axvisor_dir: root.path().join("os/axvisor"),
            arch: "x86_64".to_string(),
            target: "x86_64-unknown-none".to_string(),
            plat_dyn: Some(false),
            smp: None,
            debug: false,
            build_info_path: config_path,
            qemu_config: None,
            uboot_config: None,
            vmconfigs: vec![],
        })
        .unwrap_err();

        assert!(
            err.to_string()
                .contains("Axvisor top-level platform features")
        );
    }

    #[test]
    fn load_cargo_config_adds_default_x86_platform_when_missing() {
        let root = tempdir().unwrap();
        let config_path = root.path().join(".build.toml");
        fs::write(
            &config_path,
            r#"
env = {}
features = ["ax-driver/virtio-blk", "ax-driver/plat-static", "ept-level-4", "fs", "vmx"]
log = "Info"
"#,
        )
        .unwrap();

        let cargo = load_cargo_config(&ResolvedAxvisorRequest {
            package: AXVISOR_PACKAGE.to_string(),
            axvisor_dir: root.path().join("os/axvisor"),
            arch: "x86_64".to_string(),
            target: "x86_64-unknown-none".to_string(),
            plat_dyn: None,
            smp: None,
            debug: false,
            build_info_path: config_path,
            qemu_config: None,
            uboot_config: None,
            vmconfigs: vec![],
        })
        .unwrap();

        assert!(cargo.features.contains(&"x86-qemu-q35".to_string()));
        assert!(!cargo.features.contains(&"dyn-plat".to_string()));
        assert!(!cargo.features.contains(&"ax-hal/x86-pc".to_string()));
    }

    #[test]
    fn load_cargo_config_keeps_loongarch_axvisor_as_elf() {
        let root = tempdir().unwrap();
        let config_path = root.path().join(".build.toml");
        fs::write(
            &config_path,
            r#"
env = {}
features = ["ept-level-4"]
log = "Info"
plat_dyn = false
"#,
        )
        .unwrap();

        let cargo = load_cargo_config(&ResolvedAxvisorRequest {
            package: AXVISOR_PACKAGE.to_string(),
            axvisor_dir: root.path().join("os/axvisor"),
            arch: "loongarch64".to_string(),
            target: "loongarch64-unknown-none-softfloat".to_string(),
            plat_dyn: None,
            smp: None,
            debug: false,
            build_info_path: config_path,
            qemu_config: None,
            uboot_config: None,
            vmconfigs: vec![],
        })
        .unwrap();

        assert!(!cargo.to_bin);
        assert!(
            cargo
                .target
                .ends_with("scripts/targets/no-pie/loongarch64-unknown-none-softfloat.json")
        );
    }
}
