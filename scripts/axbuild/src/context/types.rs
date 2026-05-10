use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::snapshot::{CommandSnapshotFile, load_snapshot, store_snapshot};
use crate::arceos::build::ArceosBuildInfo;

pub const ARCEOS_SNAPSHOT_FILE: &str = ".arceos.toml";
pub const DEFAULT_ARCEOS_ARCH: &str = "aarch64";
pub const DEFAULT_ARCEOS_TARGET: &str = "aarch64-unknown-none-softfloat";
pub const AXVISOR_SNAPSHOT_FILE: &str = ".axvisor.toml";
pub const DEFAULT_AXVISOR_ARCH: &str = "aarch64";
pub const DEFAULT_AXVISOR_TARGET: &str = "aarch64-unknown-none-softfloat";
pub const STARRY_SNAPSHOT_FILE: &str = ".starry.toml";
pub const DEFAULT_STARRY_ARCH: &str = "riscv64";
pub const DEFAULT_STARRY_TARGET: &str = "riscv64gc-unknown-none-elf";
pub const STARRY_PACKAGE: &str = "starryos";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BuildCliArgs {
    pub config: Option<PathBuf>,
    pub package: Option<String>,
    pub arch: Option<String>,
    pub target: Option<String>,
    pub plat_dyn: Option<bool>,
    pub smp: Option<usize>,
    pub debug: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StarryCliArgs {
    pub config: Option<PathBuf>,
    pub arch: Option<String>,
    pub target: Option<String>,
    pub smp: Option<usize>,
    pub debug: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AxvisorCliArgs {
    pub config: Option<PathBuf>,
    pub arch: Option<String>,
    pub target: Option<String>,
    pub plat_dyn: Option<bool>,
    pub smp: Option<usize>,
    pub debug: bool,
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
    pub arch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plat_dyn: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub smp: Option<usize>,
    #[serde(default, skip_serializing_if = "ArceosQemuSnapshot::is_empty")]
    pub qemu: ArceosQemuSnapshot,
    #[serde(default, skip_serializing_if = "ArceosUbootSnapshot::is_empty")]
    pub uboot: ArceosUbootSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedBuildRequest {
    pub package: String,
    pub arch: String,
    pub target: String,
    pub plat_dyn: Option<bool>,
    pub smp: Option<usize>,
    pub debug: bool,
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
    pub smp: Option<usize>,
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
    pub smp: Option<usize>,
    pub debug: bool,
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
    pub smp: Option<usize>,
    #[serde(default, skip_serializing_if = "StarryQemuSnapshot::is_empty")]
    pub qemu: StarryQemuSnapshot,
    #[serde(default, skip_serializing_if = "StarryUbootSnapshot::is_empty")]
    pub uboot: StarryUbootSnapshot,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedStarryRequest {
    pub package: String,
    pub arch: String,
    pub target: String,
    pub plat_dyn: Option<bool>,
    pub smp: Option<usize>,
    pub debug: bool,
    pub build_info_path: PathBuf,
    pub build_info_override: Option<ArceosBuildInfo>,
    pub qemu_config: Option<PathBuf>,
    pub uboot_config: Option<PathBuf>,
}

impl ArceosQemuSnapshot {
    pub(crate) fn is_empty(&self) -> bool {
        self.qemu_config.is_none()
    }
}

impl ArceosUbootSnapshot {
    pub(crate) fn is_empty(&self) -> bool {
        self.uboot_config.is_none()
    }
}

impl AxvisorQemuSnapshot {
    pub(crate) fn is_empty(&self) -> bool {
        self.qemu_config.is_none()
    }
}

impl AxvisorUbootSnapshot {
    pub(crate) fn is_empty(&self) -> bool {
        self.uboot_config.is_none()
    }
}

impl StarryQemuSnapshot {
    pub(crate) fn is_empty(&self) -> bool {
        self.qemu_config.is_none()
    }
}

impl StarryUbootSnapshot {
    pub(crate) fn is_empty(&self) -> bool {
        self.uboot_config.is_none()
    }
}

macro_rules! impl_snapshot_file {
    ($snapshot_ty:ty, $file_name:expr) => {
        impl CommandSnapshotFile for $snapshot_ty {
            const FILE_NAME: &'static str = $file_name;
        }

        impl $snapshot_ty {
            pub(crate) fn load(root: &std::path::Path) -> anyhow::Result<Self> {
                load_snapshot(root)
            }

            pub(crate) fn store(&self, root: &std::path::Path) -> anyhow::Result<PathBuf> {
                store_snapshot(root, self)
            }
        }
    };
}

impl_snapshot_file!(ArceosCommandSnapshot, ARCEOS_SNAPSHOT_FILE);
impl_snapshot_file!(AxvisorCommandSnapshot, AXVISOR_SNAPSHOT_FILE);
impl_snapshot_file!(StarryCommandSnapshot, STARRY_SNAPSHOT_FILE);
