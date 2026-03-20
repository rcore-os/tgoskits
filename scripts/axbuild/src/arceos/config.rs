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
    str::FromStr,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use cargo_metadata::{MetadataCommand, TargetKind};
use serde::{Deserialize, Serialize};

pub const CONFIG_FILE_NAME: &str = ".arceos.toml";
pub const AXCONFIG_FILE_NAME: &str = ".axconfig.toml";
pub const QEMU_CONFIG_FILE_NAME: &str = ".qemu.toml";
pub const OSTOOL_EXTRA_CONFIG_FILE_NAME: &str = "axbuild-ostool.toml";
pub const AVAILABLE_BOARDS: &[&str] = &["qemu-x86_64", "qemu-aarch64", "qemu-riscv64"];

/// ArceOS build configuration
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArceosConfig {
    /// Target architecture
    pub arch: Arch,

    /// Platform name
    pub platform: String,

    /// Build mode
    pub mode: BuildMode,

    /// Whether to enable dynamic platform (plat-dyn) mode.
    /// If None, auto-detect based on architecture/platform.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plat_dyn: Option<bool>,

    /// Log level
    pub log: LogLevel,

    /// Number of CPU cores
    pub smp: Option<usize>,

    /// Memory size (e.g., "128M", "1G")
    pub mem: Option<String>,

    /// ArceOS module features
    pub features: Vec<String>,

    /// Application-specific features
    pub app_features: Vec<String>,

    /// QEMU options
    pub qemu: QemuOptions,
}

impl ArceosConfig {}

impl Default for ArceosConfig {
    fn default() -> Self {
        Self {
            arch: Arch::AArch64,
            platform: "aarch64-qemu-virt".to_string(),
            mode: BuildMode::Debug,
            plat_dyn: None,
            log: LogLevel::Warn,
            smp: None,
            mem: None,
            features: vec![],
            app_features: vec![],
            qemu: QemuOptions::default(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ArceosConfigOverride {
    pub arch: Option<Arch>,
    pub platform: Option<String>,
    pub mode: Option<BuildMode>,
    pub plat_dyn: Option<bool>,
    pub log: Option<LogLevel>,
    pub smp: Option<usize>,
    pub mem: Option<String>,
    pub features: Option<Vec<String>>,
    pub app_features: Option<Vec<String>>,
    pub qemu: Option<QemuOptions>,
}

impl ArceosConfigOverride {
    pub fn apply_to(self, config: &mut ArceosConfig) {
        if let Some(arch) = self.arch {
            config.arch = arch;
        }
        if let Some(platform) = self.platform {
            config.platform = platform;
        }

        if let Some(mode) = self.mode {
            config.mode = mode;
        }
        if let Some(plat_dyn) = self.plat_dyn {
            config.plat_dyn = Some(plat_dyn);
        }
        if let Some(log) = self.log {
            config.log = log;
        }
        if let Some(smp) = self.smp {
            config.smp = Some(smp);
        }
        if let Some(mem) = self.mem {
            config.mem = Some(mem);
        }
        if let Some(features) = self.features {
            config.features = features;
        }
        if let Some(app_features) = self.app_features {
            config.app_features = app_features;
        }
        if let Some(qemu) = self.qemu {
            config.qemu = qemu;
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
    let config: ArceosConfig =
        toml::from_str(&contents).with_context(|| format!("Failed to parse {}", path.display()))?;
    Ok(config)
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

pub fn parse_qemu_options(
    blk: bool,
    disk_img: Option<String>,
    net: bool,
    net_dev: Option<String>,
    graphic: bool,
    accel: bool,
    success_regex: Vec<String>,
    fail_regex: Vec<String>,
) -> QemuOptions {
    QemuOptions {
        blk,
        disk_image: disk_img.map(PathBuf::from),
        net,
        net_dev: net_dev
            .as_deref()
            .and_then(|dev| NetDev::from_str(dev).ok())
            .unwrap_or(NetDev::User),
        graphic,
        accel,
        extra_args: vec![],
        success_regex,
        fail_regex,
    }
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

    /// Get the default machine type for QEMU
    pub fn to_qemu_machine(self) -> &'static str {
        match self {
            Arch::X86_64 => "q35",
            Arch::AArch64 => "virt",
            Arch::RiscV64 => "virt",
            Arch::LoongArch64 => "virt",
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

/// Build mode
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum BuildMode {
    /// Release build with optimizations
    Release,

    /// Debug build without optimizations
    #[default]
    Debug,
}

impl std::fmt::Display for BuildMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BuildMode::Release => write!(f, "release"),
            BuildMode::Debug => write!(f, "debug"),
        }
    }
}

impl BuildMode {
    pub fn to_string(self) -> &'static str {
        match self {
            BuildMode::Release => "release",
            BuildMode::Debug => "debug",
        }
    }
}

/// Log level
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    /// No logging
    Off,
    /// Error level only
    Error,
    /// Warning and above
    #[default]
    Warn,
    /// Info and above
    Info,
    /// Debug and above
    Debug,
    /// All messages
    Trace,
}

impl LogLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            LogLevel::Off => "off",
            LogLevel::Error => "error",
            LogLevel::Warn => "warn",
            LogLevel::Info => "info",
            LogLevel::Debug => "debug",
            LogLevel::Trace => "trace",
        }
    }
}

impl std::fmt::Display for LogLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LogLevel::Off => write!(f, "off"),
            LogLevel::Error => write!(f, "error"),
            LogLevel::Warn => write!(f, "warn"),
            LogLevel::Info => write!(f, "info"),
            LogLevel::Debug => write!(f, "debug"),
            LogLevel::Trace => write!(f, "trace"),
        }
    }
}

/// QEMU options
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct QemuOptions {
    /// Enable block device
    pub blk: bool,

    /// Disk image path
    #[serde(alias = "disk_img")]
    pub disk_image: Option<PathBuf>,

    /// Enable network
    pub net: bool,

    /// Network device type
    #[serde(alias = "net_dev")]
    pub net_dev: NetDev,

    /// Enable graphic output
    pub graphic: bool,

    /// Enable KVM/HVF hardware acceleration
    pub accel: bool,

    /// Extra QEMU arguments
    pub extra_args: Vec<String>,

    /// Regex patterns that indicate a successful QEMU run
    pub success_regex: Vec<String>,

    /// Regex patterns that indicate a failed QEMU run
    pub fail_regex: Vec<String>,
}

impl Default for QemuOptions {
    fn default() -> Self {
        Self {
            blk: false,
            disk_image: None,
            net: false,
            net_dev: NetDev::User,
            graphic: false,
            accel: false,
            extra_args: vec![],
            success_regex: vec![],
            fail_regex: vec![],
        }
    }
}

/// Network device type
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum NetDev {
    /// User-mode networking
    #[default]
    User,

    /// TAP device
    Tap,

    /// Bridge device
    Bridge,
}

impl std::str::FromStr for NetDev {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> anyhow::Result<Self> {
        match s.to_lowercase().as_str() {
            "user" => Ok(NetDev::User),
            "tap" => Ok(NetDev::Tap),
            "bridge" => Ok(NetDev::Bridge),
            _ => anyhow::bail!("Unknown network device: {}", s),
        }
    }
}

impl std::fmt::Display for NetDev {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NetDev::User => write!(f, "user"),
            NetDev::Tap => write!(f, "tap"),
            NetDev::Bridge => write!(f, "bridge"),
        }
    }
}
