use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::anyhow;
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
    crate::arceos::build::default_build_info_path_in_workspace(
        &workspace_root_from_axvisor_dir(axvisor_dir),
        AXVISOR_PACKAGE,
        target,
    )
}

pub(crate) fn load_cargo_config(request: &ResolvedAxvisorRequest) -> anyhow::Result<Cargo> {
    to_cargo_config(load_build_config(request)?, request)
}

fn to_cargo_config(
    mut config: LoadedAxvisorBuildConfig,
    request: &ResolvedAxvisorRequest,
) -> anyhow::Result<Cargo> {
    config.target = request.target.clone();
    let plat_dyn = config
        .build_info
        .effective_plat_dyn(&config.target, request.plat_dyn);
    normalize_axvisor_platform_features(&mut config.build_info.features, plat_dyn);
    let mut cargo = config.build_info.into_prepared_base_cargo_config(
        &request.package,
        &config.target,
        request.plat_dyn,
    )?;
    patch_axvisor_cargo_config(&mut cargo, request, &config.vm_configs)?;
    Ok(cargo)
}

fn patch_axvisor_cargo_config(
    cargo: &mut Cargo,
    request: &ResolvedAxvisorRequest,
    config_vmconfigs: &[PathBuf],
) -> anyhow::Result<()> {
    cargo.package = request.package.clone();
    cargo.target = request.target.clone();
    cargo.to_bin = default_axvisor_to_bin(&request.arch);
    ensure_axvisor_bin_arg(&mut cargo.args);
    cargo
        .env
        .insert("AX_ARCH".to_string(), request.arch.clone());
    cargo
        .env
        .insert("AX_TARGET".to_string(), request.target.clone());

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

    let cargo_uses_plat_dyn = cargo.features.iter().any(|f| f == "ax-std/plat-dyn");
    normalize_axvisor_platform_features(&mut cargo.features, cargo_uses_plat_dyn);
    normalize_axvisor_x86_backend_features(&mut cargo.features, &request.arch)?;
    cargo.features.sort();
    cargo.features.dedup();
    Ok(())
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

fn normalize_axvisor_platform_features(features: &mut Vec<String>, plat_dyn: bool) {
    let has_axstd_defplat = features.iter().any(|feature| feature == "ax-std/defplat");
    let has_axstd_myplat = features.iter().any(|feature| feature == "ax-std/myplat");
    let has_axstd_plat_dyn = features.iter().any(|feature| feature == "ax-std/plat-dyn");

    if has_axstd_defplat && !has_axstd_myplat {
        for feature in features.iter_mut() {
            if feature == "ax-std/defplat" {
                *feature = "ax-std/myplat".to_string();
            }
        }
    } else {
        features.retain(|feature| feature != "ax-std/defplat");
    }

    if !plat_dyn
        && !has_axstd_plat_dyn
        && !features.iter().any(|feature| feature == "ax-std/myplat")
    {
        features.push("ax-std/myplat".to_string());
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum X86VirtualizationBackend {
    Vmx,
    Svm,
}

impl X86VirtualizationBackend {
    fn feature(self) -> &'static str {
        match self {
            Self::Vmx => "vmx",
            Self::Svm => "svm",
        }
    }
}

fn normalize_axvisor_x86_backend_features(
    features: &mut Vec<String>,
    arch: &str,
) -> anyhow::Result<()> {
    normalize_axvisor_x86_backend_features_with(features, arch, detect_host_x86_backend)
}

fn normalize_axvisor_x86_backend_features_with(
    features: &mut Vec<String>,
    arch: &str,
    detect_backend: impl FnOnce() -> anyhow::Result<X86VirtualizationBackend>,
) -> anyhow::Result<()> {
    if arch != "x86_64" {
        return Ok(());
    }

    let has_vmx = features.iter().any(|feature| feature == "vmx");
    let has_svm = features.iter().any(|feature| feature == "svm");

    match (has_vmx, has_svm) {
        (true, true) => Err(anyhow!(
            "x86_64 Axvisor features `vmx` and `svm` are mutually exclusive"
        )),
        (true, false) | (false, true) => Ok(()),
        (false, false) => {
            let backend = detect_backend()?;
            println!(
                "Auto-selected x86_64 virtualization backend: {}",
                backend.feature()
            );
            features.push(backend.feature().to_string());
            Ok(())
        }
    }
}

fn detect_host_x86_backend() -> anyhow::Result<X86VirtualizationBackend> {
    if let Ok(value) = std::env::var("AXVISOR_X86_BACKEND") {
        return parse_x86_backend(&value);
    }

    detect_host_x86_backend_from_cpuid()
}

fn parse_x86_backend(value: &str) -> anyhow::Result<X86VirtualizationBackend> {
    match value.trim().to_ascii_lowercase().as_str() {
        "vmx" | "intel" => Ok(X86VirtualizationBackend::Vmx),
        "svm" | "amd" => Ok(X86VirtualizationBackend::Svm),
        other => Err(anyhow!(
            "invalid AXVISOR_X86_BACKEND value `{other}`; expected `vmx`/`intel` or `svm`/`amd`"
        )),
    }
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
fn detect_host_x86_backend_from_cpuid() -> anyhow::Result<X86VirtualizationBackend> {
    #[cfg(target_arch = "x86")]
    use std::arch::x86::__cpuid;
    #[cfg(target_arch = "x86_64")]
    use std::arch::x86_64::__cpuid;

    let leaf = __cpuid(0);
    let mut bytes = [0_u8; 12];
    bytes[0..4].copy_from_slice(&leaf.ebx.to_le_bytes());
    bytes[4..8].copy_from_slice(&leaf.edx.to_le_bytes());
    bytes[8..12].copy_from_slice(&leaf.ecx.to_le_bytes());
    let vendor = String::from_utf8_lossy(&bytes).into_owned();

    match vendor.as_str() {
        "GenuineIntel" => Ok(X86VirtualizationBackend::Vmx),
        "AuthenticAMD" => Ok(X86VirtualizationBackend::Svm),
        _ => Err(anyhow!(
            "unsupported x86 CPU vendor `{vendor}` for automatic Axvisor backend selection; set \
             AXVISOR_X86_BACKEND=vmx or AXVISOR_X86_BACKEND=svm to override"
        )),
    }
}

#[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
fn detect_host_x86_backend_from_cpuid() -> anyhow::Result<X86VirtualizationBackend> {
    Err(anyhow!(
        "cannot auto-select x86_64 Axvisor virtualization backend on non-x86 host; set \
         AXVISOR_X86_BACKEND=vmx or AXVISOR_X86_BACKEND=svm"
    ))
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
            if let Some(smp) = request.smp {
                loaded.build_info.max_cpu_num = Some(smp);
            }
            return Ok(loaded);
        }

        let default_build_info = AxvisorBuildInfo::default_axvisor_for_target(&request.target);
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
        if let Some(smp) = request.smp {
            loaded.build_info.max_cpu_num = Some(smp);
        }
        return Ok(loaded);
    }

    if request.build_info_path.exists() {
        return toml::from_str::<AxvisorBuildInfo>(&content)
            .map(|build_info| {
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
                .join("target/axbuild/config/axvisor/build-aarch64-unknown-none-softfloat.toml")
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
                .join("target/axbuild/config/axvisor/build-aarch64-unknown-none-softfloat.toml")
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
features = ["ept-level-4", "ax-std/bus-mmio"]
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
        assert!(cargo.features.contains(&"ax-std/bus-mmio".to_string()));
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
        assert_eq!(cargo.target, "aarch64-unknown-none-softfloat");
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
        assert!(cargo.features.contains(&"ax-std/myplat".to_string()));
    }

    #[test]
    fn x86_backend_auto_selects_vmx_when_missing() {
        let mut features = vec!["ept-level-4".to_string(), "fs".to_string()];

        normalize_axvisor_x86_backend_features_with(&mut features, "x86_64", || {
            Ok(X86VirtualizationBackend::Vmx)
        })
        .unwrap();

        assert!(features.contains(&"vmx".to_string()));
        assert!(!features.contains(&"svm".to_string()));
    }

    #[test]
    fn x86_backend_auto_selects_svm_when_missing() {
        let mut features = vec!["ept-level-4".to_string(), "fs".to_string()];

        normalize_axvisor_x86_backend_features_with(&mut features, "x86_64", || {
            Ok(X86VirtualizationBackend::Svm)
        })
        .unwrap();

        assert!(features.contains(&"svm".to_string()));
        assert!(!features.contains(&"vmx".to_string()));
    }

    #[test]
    fn x86_backend_keeps_explicit_choice() {
        let mut features = vec!["ept-level-4".to_string(), "svm".to_string()];

        normalize_axvisor_x86_backend_features_with(&mut features, "x86_64", || {
            Ok(X86VirtualizationBackend::Vmx)
        })
        .unwrap();

        assert!(features.contains(&"svm".to_string()));
        assert!(!features.contains(&"vmx".to_string()));
    }

    #[test]
    fn x86_backend_rejects_conflicting_features() {
        let mut features = vec!["vmx".to_string(), "svm".to_string()];

        let err = normalize_axvisor_x86_backend_features_with(&mut features, "x86_64", || {
            Ok(X86VirtualizationBackend::Vmx)
        })
        .unwrap_err();

        assert!(err.to_string().contains("mutually exclusive"));
    }

    #[test]
    fn x86_backend_ignores_other_arches() {
        let mut features = vec![];

        normalize_axvisor_x86_backend_features_with(&mut features, "aarch64", || {
            Ok(X86VirtualizationBackend::Vmx)
        })
        .unwrap();

        assert!(features.is_empty());
    }

    #[test]
    fn parses_x86_backend_override() {
        assert_eq!(
            parse_x86_backend("intel").unwrap(),
            X86VirtualizationBackend::Vmx
        );
        assert_eq!(
            parse_x86_backend("svm").unwrap(),
            X86VirtualizationBackend::Svm
        );
        assert!(parse_x86_backend("unknown").is_err());
    }

    #[test]
    fn load_cargo_config_replaces_axstd_defplat_with_myplat() {
        let root = tempdir().unwrap();
        let config_path = root.path().join(".build.toml");
        fs::write(
            &config_path,
            r#"
env = {}
features = ["ax-std", "ax-std/defplat", "ept-level-4"]
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
            smp: None,
            debug: false,
            build_info_path: config_path,
            qemu_config: None,
            uboot_config: None,
            vmconfigs: vec![],
        })
        .unwrap();

        assert!(!cargo.features.contains(&"ax-std/defplat".to_string()));
        assert!(cargo.features.contains(&"ax-std/myplat".to_string()));
        assert!(cargo.args.iter().any(|arg| arg.contains("-Tlinker.x")));
    }

    #[test]
    fn load_cargo_config_keeps_loongarch_axvisor_as_elf() {
        let root = tempdir().unwrap();
        let config_path = root.path().join(".build.toml");
        fs::write(
            &config_path,
            r#"
env = {}
features = ["ept-level-4", "ax-std/bus-mmio"]
log = "Info"
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
        assert!(cargo.args.iter().any(|arg| arg.contains("-Tlinker.x")));
    }
}
