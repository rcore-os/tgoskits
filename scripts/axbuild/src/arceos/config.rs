// Copyright 2025 The tgoskits Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use cargo_metadata::{MetadataCommand, TargetKind};
pub use ostool::build::config::LogLevel;
use serde::{Deserialize, Serialize};

pub const CONFIG_FILE_NAME: &str = ".arceos.toml";
pub const AXCONFIG_FILE_NAME: &str = ".axconfig.toml";
pub const QEMU_CONFIG_FILE_NAME: &str = ".qemu.toml";
pub const OSTOOL_EXTRA_CONFIG_FILE_NAME: &str = "axbuild-ostool.toml";
pub const AVAILABLE_BOARDS: &[&str] = &["qemu-x86_64", "qemu-aarch64", "qemu-riscv64"];

/// ArceOS build configuration
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ArceosConfig {
    #[serde(flatten)]
    pub common: CommonBuildConfig,

    /// Whether to enable dynamic platform (plat-dyn) mode.
    /// If None, auto-detect based on architecture.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plat_dyn: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CommonBuildConfig {
    /// Cargo target triple.
    pub target: String,
    /// Features to enable.
    #[serde(default)]
    pub features: Vec<String>,
    /// Optional log level feature.
    #[serde(default)]
    pub log: Option<LogLevel>,
    /// Additional cargo arguments.
    #[serde(default)]
    pub cargo_args: Vec<String>,
    /// Number of CPU cores.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub smp: Option<usize>,
}

impl Default for CommonBuildConfig {
    fn default() -> Self {
        Self {
            target: Arch::AArch64.to_target().to_string(),
            features: vec![],
            log: Some(LogLevel::Warn),
            cargo_args: vec![],
            smp: None,
        }
    }
}

impl ArceosConfig {
    pub fn arch(&self) -> Result<Arch> {
        Arch::from_target_triple(&self.common.target)
    }
}

impl Default for ArceosConfig {
    fn default() -> Self {
        Self {
            common: CommonBuildConfig::default(),
            plat_dyn: None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ArceosConfigOverride {
    pub target: Option<String>,
    pub arch: Option<Arch>,
    pub plat_dyn: Option<bool>,
    pub log: Option<LogLevel>,
    pub cargo_args: Option<Vec<String>>,
    pub smp: Option<usize>,
    pub features: Option<Vec<String>>,
}

impl ArceosConfigOverride {
    pub fn apply_to(self, config: &mut ArceosConfig) {
        if let Some(target) = self.target {
            config.common.target = target;
        }
        if let Some(arch) = self.arch {
            config.common.target = arch.to_target().to_string();
        }
        if let Some(plat_dyn) = self.plat_dyn {
            config.plat_dyn = Some(plat_dyn);
        }
        if let Some(log) = self.log {
            config.common.log = Some(log);
        }
        if let Some(cargo_args) = self.cargo_args {
            config.common.cargo_args = cargo_args;
        }
        if let Some(smp) = self.smp {
            config.common.smp = Some(smp);
        }
        if let Some(features) = self.features {
            config.common.features = features;
        }
    }
}

pub fn config_path(manifest_dir: &Path) -> PathBuf {
    manifest_dir.join(CONFIG_FILE_NAME)
}

pub fn axconfig_path(manifest_dir: &Path) -> PathBuf {
    manifest_dir.join(AXCONFIG_FILE_NAME)
}

pub fn ostool_extra_config_path(manifest_dir: &Path) -> PathBuf {
    manifest_dir
        .join(".cargo")
        .join(OSTOOL_EXTRA_CONFIG_FILE_NAME)
}

pub fn load_config(manifest_dir: &Path, overrides: ArceosConfigOverride) -> Result<ArceosConfig> {
    let path = config_path(manifest_dir);
    let mut config = if path.exists() {
        let contents = fs::read_to_string(&path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        reject_legacy_arceos_fields(&contents, &path)?;
        toml::from_str(&contents).with_context(|| format!("Failed to parse {}", path.display()))?
    } else {
        ArceosConfig::default()
    };

    overrides.apply_to(&mut config);
    Ok(config)
}

pub fn save_config(manifest_dir: &Path, config: &ArceosConfig) -> Result<PathBuf> {
    let path = config_path(manifest_dir);
    let contents = toml::to_string_pretty(config)
        .with_context(|| format!("Failed to serialize {}", path.display()))?;
    fs::write(&path, contents).with_context(|| format!("Failed to write {}", path.display()))?;
    Ok(path)
}

pub fn apply_defconfig(manifest_dir: &Path, board_name: &str) -> Result<ArceosConfig> {
    let board_config = load_board_config(manifest_dir, board_name)?;
    backup_existing_config(&config_path(manifest_dir))?;
    save_config(manifest_dir, &board_config)?;
    Ok(board_config)
}

pub fn load_board_config(manifest_dir: &Path, board_name: &str) -> Result<ArceosConfig> {
    let path = resolve_board_config_path(manifest_dir, board_name)?;
    let contents =
        fs::read_to_string(&path).with_context(|| format!("Failed to read {}", path.display()))?;
    reject_legacy_arceos_fields(&contents, &path)?;
    let config: ArceosConfig =
        toml::from_str(&contents).with_context(|| format!("Failed to parse {}", path.display()))?;
    Ok(config)
}

fn reject_legacy_arceos_fields(contents: &str, path: &Path) -> Result<()> {
    let value: toml::Value = toml::from_str(contents)
        .with_context(|| format!("Failed to parse {}", path.display()))?;
    let Some(table) = value.as_table() else {
        return Ok(());
    };
    let legacy_fields = [
        "arch",
        "platform",
        "mode",
        "mem",
        "qemu",
        "app_features",
    ];
    let found = legacy_fields
        .iter()
        .copied()
        .filter(|field| table.contains_key(*field))
        .collect::<Vec<_>>();
    if !found.is_empty() {
        bail!(
            "legacy ArceOS config fields in {} are no longer supported: {}. \
             Migrate to common fields `target/features/log/cargo_args/smp` + `plat_dyn`, and put \
             runtime QEMU options in external qemu*.toml files.",
            path.display(),
            found.join(", ")
        );
    }
    Ok(())
}

pub fn resolve_package_app_dir(manifest_dir: &Path, package_name: &str) -> Result<PathBuf> {
    let mut probe_dirs = vec![manifest_dir.to_path_buf()];
    let mut cursor = manifest_dir.parent();
    while let Some(parent) = cursor {
        probe_dirs.push(parent.to_path_buf());
        cursor = parent.parent();
    }

    for dir in probe_dirs {
        let workspace_manifest = dir.join("Cargo.toml");
        if !workspace_manifest.exists() {
            continue;
        }

        if let Ok(package_dir) =
            resolve_package_app_dir_from_manifest(&workspace_manifest, package_name)
        {
            return Ok(make_path_relative(manifest_dir, &package_dir));
        }
    }

    anyhow::bail!(
        "workspace package `{}` not found; only exact package names are supported",
        package_name
    );
}

fn resolve_package_app_dir_from_manifest(
    workspace_manifest: &Path,
    package_name: &str,
) -> Result<PathBuf> {
    let metadata = MetadataCommand::new()
        .manifest_path(workspace_manifest)
        .no_deps()
        .exec()
        .with_context(|| {
            format!(
                "failed to load cargo metadata for package resolution from {}",
                workspace_manifest.display()
            )
        })?;

    let workspace_members: HashSet<_> = metadata.workspace_members.iter().cloned().collect();
    let candidates: Vec<_> = metadata
        .packages
        .iter()
        .filter(|pkg| workspace_members.contains(&pkg.id) && pkg.name == package_name)
        .collect();

    if candidates.is_empty() {
        anyhow::bail!("package `{}` not found in workspace", package_name);
    }

    if candidates.len() > 1 {
        let manifest_paths = candidates
            .iter()
            .map(|pkg| pkg.manifest_path.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        anyhow::bail!(
            "found multiple packages named `{}`: {}",
            package_name,
            manifest_paths
        );
    }

    let package = candidates[0];
    let has_bin_target = package
        .targets
        .iter()
        .any(|target| target.kind.iter().any(|kind| kind == &TargetKind::Bin));
    if !has_bin_target {
        anyhow::bail!(
            "package `{}` has no binary target; `arceos build/run -p` only supports runnable \
             packages",
            package_name
        );
    }

    let manifest_path = package.manifest_path.clone().into_std_path_buf();
    let package_dir = manifest_path.parent().with_context(|| {
        format!(
            "failed to resolve package directory from manifest path {}",
            manifest_path.display()
        )
    })?;

    Ok(package_dir.to_path_buf())
}

fn resolve_board_config_path(manifest_dir: &Path, board_name: &str) -> Result<PathBuf> {
    let file_name = if board_name.ends_with(".toml") {
        board_name.to_string()
    } else {
        format!("{board_name}.toml")
    };
    let path = manifest_dir.join("configs/board").join(file_name);
    if !path.exists() {
        anyhow::bail!(
            "Board configuration '{}' not found at {}\nAvailable boards: {}",
            board_name,
            path.display(),
            AVAILABLE_BOARDS.join(", ")
        );
    }
    Ok(path)
}

fn backup_existing_config(config_path: &Path) -> Result<()> {
    if !config_path.exists() {
        return Ok(());
    }

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before UNIX_EPOCH")?
        .as_secs();
    let backup_path = config_path.with_extension(format!("toml.backup_{timestamp}"));
    fs::copy(config_path, &backup_path).with_context(|| {
        format!(
            "Failed to backup {} to {}",
            config_path.display(),
            backup_path.display()
        )
    })?;
    Ok(())
}

fn make_path_relative(manifest_dir: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        if let Ok(relative) = path.strip_prefix(manifest_dir) {
            let relative = if relative.as_os_str().is_empty() {
                Path::new(".")
            } else {
                relative
            };
            return relative.to_path_buf();
        }
        return path.to_path_buf();
    }

    if path.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        path.to_path_buf()
    }
}

/// Supported target architectures
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Arch {
    /// x86_64 (Intel/AMD 64-bit)
    #[serde(alias = "x86_64")]
    X86_64,

    /// AArch64 (ARM 64-bit)
    #[serde(alias = "aarch64")]
    #[default]
    AArch64,

    /// RISC-V 64-bit
    #[serde(alias = "riscv64")]
    RiscV64,

    /// LoongArch 64-bit
    #[serde(alias = "loongarch64")]
    LoongArch64,
}

impl Arch {
    pub fn from_target_triple(target: &str) -> anyhow::Result<Self> {
        match target {
            "x86_64-unknown-none" => Ok(Arch::X86_64),
            "aarch64-unknown-none-softfloat" => Ok(Arch::AArch64),
            "riscv64gc-unknown-none-elf" => Ok(Arch::RiscV64),
            "loongarch64-unknown-none-softfloat" => Ok(Arch::LoongArch64),
            other => anyhow::bail!("Unknown target triple: {}", other),
        }
    }

    /// Convert to target triple
    pub fn to_target(self) -> &'static str {
        match self {
            Arch::X86_64 => "x86_64-unknown-none",
            Arch::AArch64 => "aarch64-unknown-none-softfloat",
            Arch::RiscV64 => "riscv64gc-unknown-none-elf",
            Arch::LoongArch64 => "loongarch64-unknown-none-softfloat",
        }
    }

    /// Convert to QEMU architecture name
    pub fn to_qemu_arch(self) -> &'static str {
        match self {
            Arch::X86_64 => "x86_64",
            Arch::AArch64 => "aarch64",
            Arch::RiscV64 => "riscv64",
            Arch::LoongArch64 => "loongarch64",
        }
    }

}

impl std::fmt::Display for Arch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Arch::X86_64 => write!(f, "x86_64"),
            Arch::AArch64 => write!(f, "aarch64"),
            Arch::RiscV64 => write!(f, "riscv64"),
            Arch::LoongArch64 => write!(f, "loongarch64"),
        }
    }
}

impl std::str::FromStr for Arch {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> anyhow::Result<Self> {
        match s.to_lowercase().as_str() {
            "x86_64" | "x86" => Ok(Arch::X86_64),
            "aarch64" | "arm64" => Ok(Arch::AArch64),
            "riscv64" | "riscv" => Ok(Arch::RiscV64),
            "loongarch64" | "loongarch" => Ok(Arch::LoongArch64),
            _ => anyhow::bail!("Unknown architecture: {}", s),
        }
    }
}
