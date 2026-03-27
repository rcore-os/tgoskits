use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use anyhow::{Context, anyhow};
use ostool::{
    Tool, ToolConfig,
    build::{CargoQemuAppendArgs, CargoQemuOverrideArgs, CargoRunnerKind, config::Cargo},
};
use serde::{Deserialize, Serialize};

pub const ARCEOS_SNAPSHOT_FILE: &str = ".arceos.toml";
pub const DEFAULT_ARCEOS_TARGET: &str = "aarch64-unknown-none-softfloat";
pub const AXVISOR_SNAPSHOT_FILE: &str = ".axvisor.toml";
pub const DEFAULT_AXVISOR_ARCH: &str = "aarch64";
pub const DEFAULT_AXVISOR_TARGET: &str = "aarch64-unknown-none-softfloat";
pub const STARRY_SNAPSHOT_FILE: &str = ".starry.toml";
pub const DEFAULT_STARRY_ARCH: &str = "aarch64";
pub const DEFAULT_STARRY_TARGET: &str = "aarch64-unknown-none-softfloat";
pub const STARRY_PACKAGE: &str = "starryos";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BuildCliArgs {
    pub config: Option<PathBuf>,
    pub package: Option<String>,
    pub target: Option<String>,
    pub plat_dyn: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StarryCliArgs {
    pub config: Option<PathBuf>,
    pub arch: Option<String>,
    pub target: Option<String>,
    pub plat_dyn: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AxvisorCliArgs {
    pub config: Option<PathBuf>,
    pub arch: Option<String>,
    pub target: Option<String>,
    pub plat_dyn: Option<bool>,
    pub vmconfigs: Vec<PathBuf>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArceosQemuSnapshot {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub qemu_config: Option<PathBuf>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArceosUbootSnapshot {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uboot_config: Option<PathBuf>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArceosCommandSnapshot {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plat_dyn: Option<bool>,
    #[serde(default, skip_serializing_if = "ArceosQemuSnapshot::is_empty")]
    pub qemu: ArceosQemuSnapshot,
    #[serde(default, skip_serializing_if = "ArceosUbootSnapshot::is_empty")]
    pub uboot: ArceosUbootSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedBuildRequest {
    pub package: String,
    pub target: String,
    pub plat_dyn: Option<bool>,
    pub build_info_path: PathBuf,
    pub qemu_config: Option<PathBuf>,
    pub uboot_config: Option<PathBuf>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AxvisorQemuSnapshot {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub qemu_config: Option<PathBuf>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AxvisorUbootSnapshot {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uboot_config: Option<PathBuf>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AxvisorCommandSnapshot {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plat_dyn: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub vmconfigs: Vec<PathBuf>,
    #[serde(default, skip_serializing_if = "AxvisorQemuSnapshot::is_empty")]
    pub qemu: AxvisorQemuSnapshot,
    #[serde(default, skip_serializing_if = "AxvisorUbootSnapshot::is_empty")]
    pub uboot: AxvisorUbootSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedAxvisorRequest {
    pub package: String,
    pub axvisor_dir: PathBuf,
    pub arch: String,
    pub target: String,
    pub plat_dyn: Option<bool>,
    pub build_info_path: PathBuf,
    pub qemu_config: Option<PathBuf>,
    pub uboot_config: Option<PathBuf>,
    pub vmconfigs: Vec<PathBuf>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StarryQemuSnapshot {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub qemu_config: Option<PathBuf>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StarryUbootSnapshot {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uboot_config: Option<PathBuf>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StarryCommandSnapshot {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plat_dyn: Option<bool>,
    #[serde(default, skip_serializing_if = "StarryQemuSnapshot::is_empty")]
    pub qemu: StarryQemuSnapshot,
    #[serde(default, skip_serializing_if = "StarryUbootSnapshot::is_empty")]
    pub uboot: StarryUbootSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedStarryRequest {
    pub package: String,
    pub arch: String,
    pub target: String,
    pub plat_dyn: Option<bool>,
    pub build_info_path: PathBuf,
    pub qemu_config: Option<PathBuf>,
    pub uboot_config: Option<PathBuf>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QemuRunConfig {
    pub qemu_config: Option<PathBuf>,
    pub default_args: CargoQemuOverrideArgs,
    pub append_args: CargoQemuAppendArgs,
    pub override_args: CargoQemuOverrideArgs,
}

pub struct AppContext {
    tool: Tool,
    build_config_path: Option<PathBuf>,
    root: PathBuf,
    axvisor_dir: PathBuf,
}

impl ArceosQemuSnapshot {
    fn is_empty(&self) -> bool {
        self.qemu_config.is_none()
    }
}

impl AxvisorQemuSnapshot {
    fn is_empty(&self) -> bool {
        self.qemu_config.is_none()
    }
}

impl AxvisorUbootSnapshot {
    fn is_empty(&self) -> bool {
        self.uboot_config.is_none()
    }
}

impl ArceosUbootSnapshot {
    fn is_empty(&self) -> bool {
        self.uboot_config.is_none()
    }
}

impl ArceosCommandSnapshot {
    pub fn path_in(root: &Path) -> PathBuf {
        root.join(ARCEOS_SNAPSHOT_FILE)
    }

    pub fn load(root: &Path) -> anyhow::Result<Self> {
        let path = Self::path_in(root);
        if !path.exists() {
            return Ok(Self::default());
        }

        toml::from_str(&std::fs::read_to_string(&path)?)
            .with_context(|| format!("failed to parse snapshot {}", path.display()))
    }

    pub fn store(&self, root: &Path) -> anyhow::Result<PathBuf> {
        let path = Self::path_in(root);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, toml::to_string_pretty(self)?)?;
        Ok(path)
    }
}

impl StarryQemuSnapshot {
    fn is_empty(&self) -> bool {
        self.qemu_config.is_none()
    }
}

impl StarryUbootSnapshot {
    fn is_empty(&self) -> bool {
        self.uboot_config.is_none()
    }
}

impl StarryCommandSnapshot {
    pub fn path_in(root: &Path) -> PathBuf {
        root.join(STARRY_SNAPSHOT_FILE)
    }

    pub fn load(root: &Path) -> anyhow::Result<Self> {
        let path = Self::path_in(root);
        if !path.exists() {
            return Ok(Self::default());
        }

        toml::from_str(&std::fs::read_to_string(&path)?)
            .with_context(|| format!("failed to parse snapshot {}", path.display()))
    }

    pub fn store(&self, root: &Path) -> anyhow::Result<PathBuf> {
        let path = Self::path_in(root);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, toml::to_string_pretty(self)?)?;
        Ok(path)
    }
}

impl AxvisorCommandSnapshot {
    pub fn path_in(root: &Path) -> PathBuf {
        root.join(AXVISOR_SNAPSHOT_FILE)
    }

    pub fn load(root: &Path) -> anyhow::Result<Self> {
        let path = Self::path_in(root);
        if !path.exists() {
            return Ok(Self::default());
        }

        toml::from_str(&std::fs::read_to_string(&path)?)
            .with_context(|| format!("failed to parse snapshot {}", path.display()))
    }

    pub fn store(&self, root: &Path) -> anyhow::Result<PathBuf> {
        let path = Self::path_in(root);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, toml::to_string_pretty(self)?)?;
        Ok(path)
    }
}

impl AppContext {
    pub fn new() -> anyhow::Result<Self> {
        let workspace_root = find_workspace_root();
        let axvisor_dir = workspace_member_dir(crate::axvisor::build::AXVISOR_PACKAGE)?;
        crate::logging::init_logging(&workspace_root)?;

        info!("Workspace root: {}", workspace_root.display());
        info!("Axvisor dir: {}", axvisor_dir.display());

        let tool = Tool::new(ToolConfig::default()).unwrap();
        Ok(Self {
            tool,
            build_config_path: None,
            root: workspace_root,
            axvisor_dir,
        })
    }

    pub fn workspace_root(&self) -> &Path {
        &self.root
    }

    pub fn axvisor_dir(&self) -> &Path {
        &self.axvisor_dir
    }

    pub fn prepare_arceos_request(
        &self,
        cli: BuildCliArgs,
        qemu_config: Option<PathBuf>,
        uboot_config: Option<PathBuf>,
    ) -> anyhow::Result<(ResolvedBuildRequest, ArceosCommandSnapshot)> {
        let snapshot = ArceosCommandSnapshot::load(&self.root)?;

        let package = cli
            .package
            .clone()
            .or_else(|| snapshot.package.clone())
            .ok_or_else(|| {
                anyhow!(
                    "missing ArceOS package; pass `--package` or set `package` in {}",
                    ARCEOS_SNAPSHOT_FILE
                )
            })?;
        let target = cli
            .target
            .clone()
            .or_else(|| snapshot.target.clone())
            .unwrap_or_else(|| DEFAULT_ARCEOS_TARGET.to_string());
        let plat_dyn = cli.plat_dyn.or(snapshot.plat_dyn);

        let resolved_qemu_config = qemu_config
            .clone()
            .or_else(|| resolve_snapshot_path(&self.root, snapshot.qemu.qemu_config.as_ref()));
        let resolved_uboot_config = uboot_config
            .clone()
            .or_else(|| resolve_snapshot_path(&self.root, snapshot.uboot.uboot_config.as_ref()));
        let build_info_path =
            crate::arceos::build::resolve_build_info_path(&package, &target, cli.config.clone())?;

        let request = ResolvedBuildRequest {
            package: package.clone(),
            target: target.clone(),
            plat_dyn,
            build_info_path,
            qemu_config: resolved_qemu_config.clone(),
            uboot_config: resolved_uboot_config.clone(),
        };

        let snapshot = ArceosCommandSnapshot {
            package: Some(package),
            target: Some(target),
            plat_dyn,
            qemu: ArceosQemuSnapshot {
                qemu_config: resolved_qemu_config
                    .as_ref()
                    .map(|path| snapshot_path_value(&self.root, path)),
            },
            uboot: ArceosUbootSnapshot {
                uboot_config: resolved_uboot_config
                    .as_ref()
                    .map(|path| snapshot_path_value(&self.root, path)),
            },
        };

        Ok((request, snapshot))
    }

    pub fn store_arceos_snapshot(
        &self,
        snapshot: &ArceosCommandSnapshot,
    ) -> anyhow::Result<PathBuf> {
        snapshot.store(&self.root)
    }

    pub fn prepare_starry_request(
        &self,
        cli: StarryCliArgs,
        qemu_config: Option<PathBuf>,
        uboot_config: Option<PathBuf>,
    ) -> anyhow::Result<(ResolvedStarryRequest, StarryCommandSnapshot)> {
        let snapshot = StarryCommandSnapshot::load(&self.root)?;
        let effective_arch = cli.arch.clone().or_else(|| {
            if cli.target.is_some() {
                None
            } else {
                snapshot.arch.clone()
            }
        });
        let effective_target = cli.target.clone().or_else(|| {
            if cli.arch.is_some() {
                None
            } else {
                snapshot.target.clone()
            }
        });
        let (arch, target) = resolve_starry_arch_and_target(effective_arch, effective_target)?;
        let plat_dyn = cli.plat_dyn.or(snapshot.plat_dyn);

        let resolved_qemu_config = qemu_config
            .clone()
            .or_else(|| resolve_snapshot_path(&self.root, snapshot.qemu.qemu_config.as_ref()));
        let resolved_uboot_config = uboot_config
            .clone()
            .or_else(|| resolve_snapshot_path(&self.root, snapshot.uboot.uboot_config.as_ref()));
        let build_info_path =
            crate::starry::build::resolve_build_info_path(&self.root, &target, cli.config)?;

        let request = ResolvedStarryRequest {
            package: STARRY_PACKAGE.to_string(),
            arch: arch.clone(),
            target: target.clone(),
            plat_dyn,
            build_info_path,
            qemu_config: resolved_qemu_config.clone(),
            uboot_config: resolved_uboot_config.clone(),
        };

        let snapshot = StarryCommandSnapshot {
            arch: Some(arch),
            target: Some(target),
            plat_dyn,
            qemu: StarryQemuSnapshot {
                qemu_config: resolved_qemu_config
                    .as_ref()
                    .map(|path| snapshot_path_value(&self.root, path)),
            },
            uboot: StarryUbootSnapshot {
                uboot_config: resolved_uboot_config
                    .as_ref()
                    .map(|path| snapshot_path_value(&self.root, path)),
            },
        };

        Ok((request, snapshot))
    }

    pub fn prepare_axvisor_request(
        &self,
        cli: AxvisorCliArgs,
        qemu_config: Option<PathBuf>,
        uboot_config: Option<PathBuf>,
    ) -> anyhow::Result<(ResolvedAxvisorRequest, AxvisorCommandSnapshot)> {
        let snapshot = AxvisorCommandSnapshot::load(&self.root)?;
        let explicit_config = cli
            .config
            .clone()
            .or_else(|| resolve_snapshot_path(&self.root, snapshot.config.as_ref()));
        let config_target = explicit_config
            .as_ref()
            .filter(|path| path.exists())
            .map(|path| crate::axvisor::build::load_target_from_build_config(path))
            .transpose()?
            .flatten();

        let effective_arch = cli.arch.clone().or_else(|| {
            if cli.target.is_some() || config_target.is_some() {
                None
            } else {
                snapshot.arch.clone()
            }
        });
        let effective_target = cli
            .target
            .clone()
            .or_else(|| config_target.clone())
            .or_else(|| {
                if cli.arch.is_some() {
                    None
                } else {
                    snapshot.target.clone()
                }
            });
        let (arch, target) = resolve_axvisor_arch_and_target(effective_arch, effective_target)?;
        let plat_dyn = cli.plat_dyn.or(snapshot.plat_dyn);
        let build_info_path = crate::axvisor::build::resolve_build_info_path(
            &self.axvisor_dir,
            &target,
            explicit_config,
        )?;
        let resolved_qemu_config = qemu_config
            .clone()
            .or_else(|| resolve_snapshot_path(&self.root, snapshot.qemu.qemu_config.as_ref()));
        let resolved_uboot_config = uboot_config
            .clone()
            .or_else(|| resolve_snapshot_path(&self.root, snapshot.uboot.uboot_config.as_ref()));
        let vmconfigs: Vec<PathBuf> = if cli.vmconfigs.is_empty() {
            snapshot
                .vmconfigs
                .iter()
                .map(|path| {
                    if path.is_relative() {
                        self.root.join(path)
                    } else {
                        path.clone()
                    }
                })
                .collect()
        } else {
            cli.vmconfigs
                .iter()
                .map(|path| {
                    if path.is_absolute() {
                        path.clone()
                    } else {
                        self.root.join(path)
                    }
                })
                .collect()
        };

        let request = ResolvedAxvisorRequest {
            package: crate::axvisor::build::AXVISOR_PACKAGE.to_string(),
            axvisor_dir: self.axvisor_dir.clone(),
            arch: arch.clone(),
            target: target.clone(),
            plat_dyn,
            build_info_path: build_info_path.clone(),
            qemu_config: resolved_qemu_config.clone(),
            uboot_config: resolved_uboot_config.clone(),
            vmconfigs: vmconfigs.clone(),
        };

        let snapshot = AxvisorCommandSnapshot {
            arch: Some(arch),
            target: Some(target),
            plat_dyn,
            config: Some(snapshot_path_value(&self.root, &build_info_path)),
            vmconfigs: vmconfigs
                .iter()
                .map(|path| snapshot_path_value(&self.root, path))
                .collect(),
            qemu: AxvisorQemuSnapshot {
                qemu_config: resolved_qemu_config
                    .as_ref()
                    .map(|path| snapshot_path_value(&self.root, path)),
            },
            uboot: AxvisorUbootSnapshot {
                uboot_config: resolved_uboot_config
                    .as_ref()
                    .map(|path| snapshot_path_value(&self.root, path)),
            },
        };

        Ok((request, snapshot))
    }

    pub fn store_axvisor_snapshot(
        &self,
        snapshot: &AxvisorCommandSnapshot,
    ) -> anyhow::Result<PathBuf> {
        snapshot.store(&self.root)
    }

    pub fn store_starry_snapshot(
        &self,
        snapshot: &StarryCommandSnapshot,
    ) -> anyhow::Result<PathBuf> {
        snapshot.store(&self.root)
    }

    pub async fn build(&mut self, cargo: Cargo, build_config_path: PathBuf) -> anyhow::Result<()> {
        self.set_build_config_path(build_config_path);
        self.tool.cargo_build(&cargo).await
    }

    pub async fn qemu(
        &mut self,
        cargo: Cargo,
        build_config_path: PathBuf,
        mut qemu: QemuRunConfig,
    ) -> anyhow::Result<()> {
        self.set_build_config_path(build_config_path);
        qemu.default_args.to_bin.get_or_insert(cargo.to_bin);
        self.tool
            .cargo_run(
                &cargo,
                &CargoRunnerKind::Qemu {
                    qemu_config: qemu.qemu_config,
                    debug: false,
                    dtb_dump: false,
                    default_args: qemu.default_args,
                    append_args: qemu.append_args,
                    override_args: qemu.override_args,
                },
            )
            .await
    }

    pub async fn uboot(
        &mut self,
        cargo: Cargo,
        build_config_path: PathBuf,
        uboot_config: Option<PathBuf>,
    ) -> anyhow::Result<()> {
        self.set_build_config_path(build_config_path);
        self.tool
            .cargo_run(&cargo, &CargoRunnerKind::Uboot { uboot_config })
            .await
    }

    fn set_build_config_path(&mut self, path: PathBuf) {
        self.build_config_path = Some(path.clone());
        self.tool.ctx_mut().build_config_path = Some(path);
    }
}

impl Default for AppContext {
    fn default() -> Self {
        Self::new().expect("failed to initialize AppContext")
    }
}

pub(crate) fn workspace_root_path() -> anyhow::Result<PathBuf> {
    let cargo = workspace_metadata()?;

    cargo
        .workspace_root
        .canonicalize()
        .context("failed to canonicalize workspace root")
}

pub(crate) fn workspace_member_dir(package: &str) -> anyhow::Result<PathBuf> {
    let manifest_path = workspace_member_manifest_path(package)?;
    manifest_path
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| anyhow!("package manifest path has no parent directory"))
}

fn find_workspace_root() -> PathBuf {
    workspace_root_path().expect("failed to resolve workspace root")
}

fn workspace_member_manifest_path(package: &str) -> anyhow::Result<PathBuf> {
    let metadata = workspace_metadata()?;
    let workspace_members: HashSet<_> = metadata.workspace_members.iter().cloned().collect();
    metadata
        .packages
        .iter()
        .find(|pkg| workspace_members.contains(&pkg.id) && pkg.name == package)
        .map(|pkg| pkg.manifest_path.clone().into_std_path_buf())
        .ok_or_else(|| anyhow!("workspace package `{package}` not found"))
}

fn workspace_metadata() -> anyhow::Result<cargo_metadata::Metadata> {
    cargo_metadata::MetadataCommand::new()
        .no_deps()
        .exec()
        .context("failed to get cargo metadata")
}

fn resolve_snapshot_path(root: &Path, path: Option<&PathBuf>) -> Option<PathBuf> {
    path.map(|path| {
        if path.is_relative() {
            root.join(path)
        } else {
            path.clone()
        }
    })
}

pub(crate) fn snapshot_path_value(root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.strip_prefix(root)
            .map(PathBuf::from)
            .unwrap_or_else(|_| path.to_path_buf())
    } else {
        path.to_path_buf()
    }
}

pub fn starry_target_for_arch(arch: &str) -> &'static str {
    match arch {
        "aarch64" => "aarch64-unknown-none-softfloat",
        "x86_64" => "x86_64-unknown-none",
        "riscv64" => "riscv64gc-unknown-none-elf",
        "loongarch64" => "loongarch64-unknown-none-softfloat",
        _ => panic!("unsupported Starry architecture: {arch}"),
    }
}

pub fn target_for_arch(arch: &str) -> &'static str {
    starry_target_for_arch(arch)
}

pub fn starry_arch_for_target(target: &str) -> Option<&'static str> {
    match target {
        "aarch64-unknown-none-softfloat" => Some("aarch64"),
        "x86_64-unknown-none" => Some("x86_64"),
        "riscv64gc-unknown-none-elf" => Some("riscv64"),
        "loongarch64-unknown-none-softfloat" => Some("loongarch64"),
        _ => None,
    }
}

pub fn arch_for_target(target: &str) -> Option<&'static str> {
    starry_arch_for_target(target)
}

fn validate_starry_arch_target_pair(arch: &str, target: &str) -> anyhow::Result<()> {
    let expected_target = starry_target_for_arch_checked(arch)?;
    if target != expected_target {
        anyhow::bail!(
            "Starry arch `{arch}` maps to target `{expected_target}`, but got `{target}`"
        );
    }
    Ok(())
}

fn resolve_starry_arch_and_target(
    arch: Option<String>,
    target: Option<String>,
) -> anyhow::Result<(String, String)> {
    match (arch, target) {
        (Some(arch), Some(target)) => {
            validate_starry_arch_target_pair(&arch, &target)?;
            Ok((arch, target))
        }
        (Some(arch), None) => Ok((
            arch.clone(),
            starry_target_for_arch_checked(&arch)?.to_string(),
        )),
        (None, Some(target)) => Ok((starry_arch_for_target_checked(&target)?.to_string(), target)),
        (None, None) => Ok((
            DEFAULT_STARRY_ARCH.to_string(),
            DEFAULT_STARRY_TARGET.to_string(),
        )),
    }
}

pub fn starry_target_for_arch_checked(arch: &str) -> anyhow::Result<&'static str> {
    target_for_arch_checked(arch)
}

pub fn target_for_arch_checked(arch: &str) -> anyhow::Result<&'static str> {
    match arch {
        "aarch64" | "x86_64" | "riscv64" | "loongarch64" => Ok(target_for_arch(arch)),
        _ => anyhow::bail!(
            "unsupported Starry architecture `{arch}`; expected one of aarch64, x86_64, riscv64, \
             loongarch64"
        ),
    }
}

pub fn starry_arch_for_target_checked(target: &str) -> anyhow::Result<&'static str> {
    arch_for_target_checked(target)
}

pub fn arch_for_target_checked(target: &str) -> anyhow::Result<&'static str> {
    arch_for_target(target).ok_or_else(|| {
        anyhow!(
            "unsupported Starry target `{target}`; expected one of x86_64-unknown-none, \
             aarch64-unknown-none-softfloat, riscv64gc-unknown-none-elf, \
             loongarch64-unknown-none-softfloat"
        )
    })
}

fn resolve_axvisor_arch_and_target(
    arch: Option<String>,
    target: Option<String>,
) -> anyhow::Result<(String, String)> {
    match (arch, target) {
        (Some(arch), Some(target)) => {
            let expected_target = target_for_arch_checked(&arch)?;
            if target != expected_target {
                anyhow::bail!(
                    "Axvisor arch `{arch}` maps to target `{expected_target}`, but got `{target}`"
                );
            }
            Ok((arch, target))
        }
        (Some(arch), None) => Ok((arch.clone(), target_for_arch_checked(&arch)?.to_string())),
        (None, Some(target)) => Ok((arch_for_target_checked(&target)?.to_string(), target)),
        (None, None) => Ok((
            DEFAULT_AXVISOR_ARCH.to_string(),
            DEFAULT_AXVISOR_TARGET.to_string(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    use tempfile::tempdir;

    use super::*;

    fn test_app_context(root: &Path) -> AppContext {
        AppContext {
            tool: Tool::new(ToolConfig::default()).unwrap(),
            build_config_path: None,
            root: root.to_path_buf(),
            axvisor_dir: root.join("os/axvisor"),
        }
    }

    #[test]
    fn snapshot_load_returns_default_when_missing() {
        let root = tempdir().unwrap();
        let snapshot = ArceosCommandSnapshot::load(root.path()).unwrap();
        assert_eq!(snapshot, ArceosCommandSnapshot::default());
    }

    #[test]
    fn axvisor_snapshot_load_returns_default_when_missing() {
        let root = tempdir().unwrap();
        let snapshot = AxvisorCommandSnapshot::load(root.path()).unwrap();
        assert_eq!(snapshot, AxvisorCommandSnapshot::default());
    }

    #[test]
    fn snapshot_store_round_trips() {
        let root = tempdir().unwrap();
        let snapshot = ArceosCommandSnapshot {
            package: Some("arceos-helloworld".into()),
            target: Some("target".into()),
            plat_dyn: Some(true),
            qemu: ArceosQemuSnapshot {
                qemu_config: Some(PathBuf::from("configs/qemu.toml")),
            },
            uboot: ArceosUbootSnapshot {
                uboot_config: Some(PathBuf::from("configs/uboot.toml")),
            },
        };

        let path = snapshot.store(root.path()).unwrap();
        let loaded = ArceosCommandSnapshot::load(root.path()).unwrap();

        assert_eq!(path, root.path().join(ARCEOS_SNAPSHOT_FILE));
        assert_eq!(loaded, snapshot);
    }

    #[test]
    fn axvisor_snapshot_store_round_trips() {
        let root = tempdir().unwrap();
        let snapshot = AxvisorCommandSnapshot {
            arch: Some("aarch64".into()),
            target: Some(DEFAULT_AXVISOR_TARGET.into()),
            plat_dyn: Some(false),
            config: Some(PathBuf::from("os/axvisor/.build.toml")),
            vmconfigs: vec![PathBuf::from("tmp/vm1.toml"), PathBuf::from("tmp/vm2.toml")],
            qemu: AxvisorQemuSnapshot {
                qemu_config: Some(PathBuf::from("configs/qemu.toml")),
            },
            uboot: AxvisorUbootSnapshot {
                uboot_config: Some(PathBuf::from("configs/uboot.toml")),
            },
        };

        let path = snapshot.store(root.path()).unwrap();
        let loaded = AxvisorCommandSnapshot::load(root.path()).unwrap();

        assert_eq!(path, root.path().join(AXVISOR_SNAPSHOT_FILE));
        assert_eq!(loaded, snapshot);
    }

    #[test]
    fn prepare_request_prefers_cli_over_snapshot() {
        let root = tempdir().unwrap();
        fs::write(
            root.path().join(ARCEOS_SNAPSHOT_FILE),
            r#"
package = "from-snapshot"
target = "snapshot-target"
plat_dyn = false

[qemu]
qemu_config = "configs/snapshot-qemu.toml"

[uboot]
uboot_config = "configs/snapshot-uboot.toml"
"#,
        )
        .unwrap();

        let app = test_app_context(root.path());

        let (request, snapshot) = app
            .prepare_arceos_request(
                BuildCliArgs {
                    config: Some(PathBuf::from("/tmp/custom-build.toml")),
                    package: Some("from-cli".into()),
                    target: Some("cli-target".into()),
                    plat_dyn: Some(true),
                },
                Some(PathBuf::from("/tmp/qemu.toml")),
                None,
            )
            .unwrap();

        assert_eq!(request.package, "from-cli");
        assert_eq!(request.target, "cli-target");
        assert_eq!(request.plat_dyn, Some(true));
        assert_eq!(
            request.build_info_path,
            PathBuf::from("/tmp/custom-build.toml")
        );
        assert_eq!(request.qemu_config, Some(PathBuf::from("/tmp/qemu.toml")));
        assert_eq!(
            request.uboot_config,
            Some(root.path().join("configs/snapshot-uboot.toml"))
        );
        assert_eq!(snapshot.package.as_deref(), Some("from-cli"));
        assert_eq!(snapshot.target.as_deref(), Some("cli-target"));
        assert_eq!(snapshot.plat_dyn, Some(true));
        assert_eq!(
            snapshot.qemu.qemu_config,
            Some(PathBuf::from("/tmp/qemu.toml"))
        );
    }

    #[test]
    fn prepare_request_uses_snapshot_and_default_target() {
        let root = tempdir().unwrap();
        fs::write(
            root.path().join(ARCEOS_SNAPSHOT_FILE),
            r#"
package = "arceos-helloworld"

[qemu]
qemu_config = "configs/qemu.toml"
"#,
        )
        .unwrap();

        let app = test_app_context(root.path());

        let (request, snapshot) = app
            .prepare_arceos_request(BuildCliArgs::default(), None, None)
            .unwrap();

        assert_eq!(request.package, "arceos-helloworld");
        assert_eq!(request.target, DEFAULT_ARCEOS_TARGET);
        assert_eq!(request.plat_dyn, None);
        assert_eq!(
            request.qemu_config,
            Some(root.path().join("configs/qemu.toml"))
        );
        assert_eq!(snapshot.target.as_deref(), Some(DEFAULT_ARCEOS_TARGET));
    }

    #[test]
    fn prepare_request_requires_package() {
        let root = tempdir().unwrap();
        let app = test_app_context(root.path());

        let err = app
            .prepare_arceos_request(BuildCliArgs::default(), None, None)
            .unwrap_err();

        assert!(err.to_string().contains("missing ArceOS package"));
    }

    #[test]
    fn prepare_axvisor_request_prefers_cli_over_snapshot() {
        let root = tempdir().unwrap();
        fs::write(
            root.path().join(AXVISOR_SNAPSHOT_FILE),
            r#"
config = "os/axvisor/.build.toml"
arch = "riscv64"
target = "riscv64gc-unknown-none-elf"
plat_dyn = false
vmconfigs = ["tmp/snapshot-vm.toml"]

[qemu]
qemu_config = "configs/snapshot-qemu.toml"

[uboot]
uboot_config = "configs/snapshot-uboot.toml"
"#,
        )
        .unwrap();

        let app = test_app_context(root.path());

        let (request, snapshot) = app
            .prepare_axvisor_request(
                AxvisorCliArgs {
                    config: Some(PathBuf::from("/tmp/custom-build.toml")),
                    arch: Some("aarch64".into()),
                    target: Some(DEFAULT_AXVISOR_TARGET.into()),
                    plat_dyn: Some(true),
                    vmconfigs: vec![
                        PathBuf::from("/tmp/vm1.toml"),
                        PathBuf::from("/tmp/vm2.toml"),
                    ],
                },
                Some(PathBuf::from("/tmp/qemu.toml")),
                Some(PathBuf::from("/tmp/uboot.toml")),
            )
            .unwrap();

        assert_eq!(request.package, crate::axvisor::build::AXVISOR_PACKAGE);
        assert_eq!(request.arch, DEFAULT_AXVISOR_ARCH);
        assert_eq!(request.target, DEFAULT_AXVISOR_TARGET);
        assert_eq!(request.plat_dyn, Some(true));
        assert_eq!(
            request.build_info_path,
            PathBuf::from("/tmp/custom-build.toml")
        );
        assert_eq!(request.qemu_config, Some(PathBuf::from("/tmp/qemu.toml")));
        assert_eq!(request.uboot_config, Some(PathBuf::from("/tmp/uboot.toml")));
        assert_eq!(
            request.vmconfigs,
            vec![
                PathBuf::from("/tmp/vm1.toml"),
                PathBuf::from("/tmp/vm2.toml")
            ]
        );
        assert_eq!(
            snapshot.config,
            Some(PathBuf::from("/tmp/custom-build.toml"))
        );
        assert_eq!(snapshot.arch.as_deref(), Some(DEFAULT_AXVISOR_ARCH));
        assert_eq!(snapshot.target.as_deref(), Some(DEFAULT_AXVISOR_TARGET));
        assert_eq!(snapshot.plat_dyn, Some(true));
        assert_eq!(
            snapshot.vmconfigs,
            vec![
                PathBuf::from("/tmp/vm1.toml"),
                PathBuf::from("/tmp/vm2.toml")
            ]
        );
        assert_eq!(
            snapshot.qemu.qemu_config,
            Some(PathBuf::from("/tmp/qemu.toml"))
        );
        assert_eq!(
            snapshot.uboot.uboot_config,
            Some(PathBuf::from("/tmp/uboot.toml"))
        );
    }

    #[test]
    fn prepare_axvisor_request_uses_snapshot_when_cli_omits_values() {
        let root = tempdir().unwrap();
        fs::write(
            root.path().join(AXVISOR_SNAPSHOT_FILE),
            r#"
config = "os/axvisor/.build.toml"
arch = "aarch64"
target = "aarch64-unknown-none-softfloat"
vmconfigs = ["tmp/vm1.toml", "tmp/vm2.toml"]

[qemu]
qemu_config = "configs/qemu.toml"

[uboot]
uboot_config = "configs/uboot.toml"
"#,
        )
        .unwrap();

        let app = test_app_context(root.path());

        let (request, snapshot) = app
            .prepare_axvisor_request(AxvisorCliArgs::default(), None, None)
            .unwrap();

        assert_eq!(request.arch, DEFAULT_AXVISOR_ARCH);
        assert_eq!(request.target, DEFAULT_AXVISOR_TARGET);
        assert_eq!(request.plat_dyn, None);
        assert_eq!(
            request.build_info_path,
            root.path().join("os/axvisor/.build.toml")
        );
        assert_eq!(
            request.qemu_config,
            Some(root.path().join("configs/qemu.toml"))
        );
        assert_eq!(
            request.uboot_config,
            Some(root.path().join("configs/uboot.toml"))
        );
        assert_eq!(
            request.vmconfigs,
            vec![
                root.path().join("tmp/vm1.toml"),
                root.path().join("tmp/vm2.toml")
            ]
        );
        assert_eq!(
            snapshot.config,
            Some(PathBuf::from("os/axvisor/.build.toml"))
        );
        assert_eq!(snapshot.arch.as_deref(), Some(DEFAULT_AXVISOR_ARCH));
        assert_eq!(snapshot.target.as_deref(), Some(DEFAULT_AXVISOR_TARGET));
        assert_eq!(
            snapshot.vmconfigs,
            vec![PathBuf::from("tmp/vm1.toml"), PathBuf::from("tmp/vm2.toml")]
        );
        assert_eq!(
            snapshot.uboot.uboot_config,
            Some(PathBuf::from("configs/uboot.toml"))
        );
    }

    #[test]
    fn prepare_axvisor_request_resolves_target_from_arch() {
        let root = tempdir().unwrap();
        let app = test_app_context(root.path());

        let (request, snapshot) = app
            .prepare_axvisor_request(
                AxvisorCliArgs {
                    config: None,
                    arch: Some("x86_64".into()),
                    target: None,
                    plat_dyn: None,
                    vmconfigs: vec![],
                },
                None,
                None,
            )
            .unwrap();

        assert_eq!(request.arch, "x86_64");
        assert_eq!(request.target, "x86_64-unknown-none");
        assert_eq!(
            request.build_info_path,
            root.path()
                .join("os/axvisor/.build-x86_64-unknown-none.toml")
        );
        assert_eq!(snapshot.arch.as_deref(), Some("x86_64"));
        assert_eq!(snapshot.target.as_deref(), Some("x86_64-unknown-none"));
    }

    #[test]
    fn starry_snapshot_load_returns_default_when_missing() {
        let root = tempdir().unwrap();
        let snapshot = StarryCommandSnapshot::load(root.path()).unwrap();
        assert_eq!(snapshot, StarryCommandSnapshot::default());
    }

    #[test]
    fn starry_snapshot_store_round_trips() {
        let root = tempdir().unwrap();
        let snapshot = StarryCommandSnapshot {
            arch: Some("aarch64".into()),
            target: Some(DEFAULT_STARRY_TARGET.into()),
            plat_dyn: Some(false),
            qemu: StarryQemuSnapshot {
                qemu_config: Some(PathBuf::from("configs/qemu.toml")),
            },
            uboot: StarryUbootSnapshot {
                uboot_config: Some(PathBuf::from("configs/uboot.toml")),
            },
        };

        let path = snapshot.store(root.path()).unwrap();
        let loaded = StarryCommandSnapshot::load(root.path()).unwrap();

        assert_eq!(path, root.path().join(STARRY_SNAPSHOT_FILE));
        assert_eq!(loaded, snapshot);
    }

    #[test]
    fn prepare_starry_request_prefers_cli_over_snapshot() {
        let root = tempdir().unwrap();
        fs::write(
            root.path().join(STARRY_SNAPSHOT_FILE),
            r#"
arch = "riscv64"
target = "riscv64gc-unknown-none-elf"
plat_dyn = false

[qemu]
qemu_config = "configs/snapshot-qemu.toml"

[uboot]
uboot_config = "configs/snapshot-uboot.toml"
"#,
        )
        .unwrap();

        let app = test_app_context(root.path());

        let (request, snapshot) = app
            .prepare_starry_request(
                StarryCliArgs {
                    config: Some(PathBuf::from("/tmp/starry-build.toml")),
                    arch: Some("aarch64".into()),
                    target: Some(DEFAULT_STARRY_TARGET.into()),
                    plat_dyn: Some(true),
                },
                Some(PathBuf::from("/tmp/qemu.toml")),
                None,
            )
            .unwrap();

        assert_eq!(request.package, STARRY_PACKAGE);
        assert_eq!(request.arch, DEFAULT_STARRY_ARCH);
        assert_eq!(request.target, DEFAULT_STARRY_TARGET);
        assert_eq!(request.plat_dyn, Some(true));
        assert_eq!(
            request.build_info_path,
            PathBuf::from("/tmp/starry-build.toml")
        );
        assert_eq!(request.qemu_config, Some(PathBuf::from("/tmp/qemu.toml")));
        assert_eq!(
            request.uboot_config,
            Some(root.path().join("configs/snapshot-uboot.toml"))
        );
        assert_eq!(snapshot.arch.as_deref(), Some(DEFAULT_STARRY_ARCH));
        assert_eq!(snapshot.target.as_deref(), Some(DEFAULT_STARRY_TARGET));
        assert_eq!(snapshot.plat_dyn, Some(true));
    }

    #[test]
    fn prepare_starry_request_uses_snapshot_and_default_arch() {
        let root = tempdir().unwrap();
        fs::write(
            root.path().join(STARRY_SNAPSHOT_FILE),
            r#"
[qemu]
qemu_config = "configs/qemu.toml"
"#,
        )
        .unwrap();

        let app = test_app_context(root.path());

        let (request, snapshot) = app
            .prepare_starry_request(StarryCliArgs::default(), None, None)
            .unwrap();

        assert_eq!(request.package, STARRY_PACKAGE);
        assert_eq!(request.arch, DEFAULT_STARRY_ARCH);
        assert_eq!(request.target, DEFAULT_STARRY_TARGET);
        assert_eq!(request.plat_dyn, None);
        assert_eq!(
            request.build_info_path,
            root.path()
                .join("os/StarryOS/starryos/.build-aarch64-unknown-none-softfloat.toml")
        );
        assert_eq!(
            request.qemu_config,
            Some(root.path().join("configs/qemu.toml"))
        );
        assert_eq!(snapshot.arch.as_deref(), Some(DEFAULT_STARRY_ARCH));
        assert_eq!(snapshot.target.as_deref(), Some(DEFAULT_STARRY_TARGET));
    }

    #[test]
    fn prepare_starry_request_rejects_mismatched_arch_and_target() {
        let root = tempdir().unwrap();
        let app = test_app_context(root.path());

        let err = app
            .prepare_starry_request(
                StarryCliArgs {
                    config: None,
                    arch: Some("aarch64".into()),
                    target: Some("x86_64-unknown-none".into()),
                    plat_dyn: None,
                },
                None,
                None,
            )
            .unwrap_err();

        assert!(err.to_string().contains("maps to target"));
    }

    #[test]
    fn prepare_starry_request_cli_arch_overrides_snapshot_target() {
        let root = tempdir().unwrap();
        fs::write(
            root.path().join(STARRY_SNAPSHOT_FILE),
            r#"
arch = "aarch64"
target = "aarch64-unknown-none-softfloat"
"#,
        )
        .unwrap();

        let app = test_app_context(root.path());

        let (request, snapshot) = app
            .prepare_starry_request(
                StarryCliArgs {
                    config: None,
                    arch: Some("riscv64".into()),
                    target: None,
                    plat_dyn: None,
                },
                None,
                None,
            )
            .unwrap();

        assert_eq!(request.arch, "riscv64");
        assert_eq!(request.target, "riscv64gc-unknown-none-elf");
        assert_eq!(snapshot.arch.as_deref(), Some("riscv64"));
        assert_eq!(
            snapshot.target.as_deref(),
            Some("riscv64gc-unknown-none-elf")
        );
    }

    #[test]
    fn prepare_starry_request_cli_target_overrides_snapshot_arch() {
        let root = tempdir().unwrap();
        fs::write(
            root.path().join(STARRY_SNAPSHOT_FILE),
            r#"
arch = "aarch64"
target = "aarch64-unknown-none-softfloat"
"#,
        )
        .unwrap();

        let app = test_app_context(root.path());

        let (request, snapshot) = app
            .prepare_starry_request(
                StarryCliArgs {
                    config: None,
                    arch: None,
                    target: Some("x86_64-unknown-none".into()),
                    plat_dyn: None,
                },
                None,
                None,
            )
            .unwrap();

        assert_eq!(request.arch, "x86_64");
        assert_eq!(request.target, "x86_64-unknown-none");
        assert_eq!(snapshot.arch.as_deref(), Some("x86_64"));
        assert_eq!(snapshot.target.as_deref(), Some("x86_64-unknown-none"));
    }

    #[test]
    fn starry_arch_target_mapping_helpers_work() {
        assert_eq!(
            starry_target_for_arch_checked("aarch64").unwrap(),
            DEFAULT_STARRY_TARGET
        );
        assert_eq!(
            starry_arch_for_target_checked("x86_64-unknown-none").unwrap(),
            "x86_64"
        );
        assert!(starry_target_for_arch_checked("mips64").is_err());
        assert!(starry_arch_for_target_checked("mips64-unknown-none").is_err());
    }

    #[test]
    fn resolve_starry_arch_and_target_infers_arch_from_target() {
        let (arch, target) =
            resolve_starry_arch_and_target(None, Some("x86_64-unknown-none".into())).unwrap();

        assert_eq!(arch, "x86_64");
        assert_eq!(target, "x86_64-unknown-none");
    }
}
