use std::path::{Path, PathBuf};

use ax_config::{RuntimeEnv, RuntimeValues};
use serde::{Deserialize, Serialize};

use super::snapshot::{CommandSnapshotFile, load_snapshot, store_snapshot};
use crate::{
    arceos::build::ArceosConfig,
    axvisor::build::AxvisorConfig,
    build::{ConfigInput, ConfigSource},
    starry::build::StarryConfig,
};

pub const ARCEOS_SNAPSHOT_FILE: &str = ".arceos.toml";
pub const DEFAULT_ARCEOS_ARCH: &str = "aarch64";
pub const DEFAULT_ARCEOS_TARGET: &str = ax_config::boards::TARGET_AARCH64;
pub const AXVISOR_SNAPSHOT_FILE: &str = ".axvisor.toml";
pub const DEFAULT_AXVISOR_ARCH: &str = "aarch64";
pub const DEFAULT_AXVISOR_TARGET: &str = ax_config::boards::TARGET_AARCH64;
pub const STARRY_SNAPSHOT_FILE: &str = ".starry.toml";
pub const DEFAULT_STARRY_ARCH: &str = "riscv64";
pub const DEFAULT_STARRY_TARGET: &str = ax_config::boards::TARGET_RISCV64;
pub const STARRY_PACKAGE: &str = "starryos";
pub const STARRY_KERNEL_PACKAGE: &str = "starry-kernel";

/// Environment captured before an application discovers and decodes its input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RequestEnvironment {
    pub runtime: RuntimeEnv,
    pub features: Vec<String>,
}

impl RequestEnvironment {
    pub fn capture() -> anyhow::Result<Self> {
        Ok(Self {
            runtime: RuntimeEnv::capture()?,
            features: crate::build::makefile_features_from_env(),
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BuildCliArgs {
    pub preset: Option<String>,
    pub config: Option<PathBuf>,
    pub package: Option<String>,
    pub arch: Option<String>,
    pub target: Option<String>,
    pub smp: Option<usize>,
    pub debug: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StarryCliArgs {
    pub preset: Option<String>,
    pub config: Option<PathBuf>,
    pub arch: Option<String>,
    pub target: Option<String>,
    pub smp: Option<usize>,
    pub debug: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AxvisorCliArgs {
    pub preset: Option<String>,
    pub config: Option<PathBuf>,
    pub arch: Option<String>,
    pub target: Option<String>,
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
    pub preset: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub smp: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "ArceosQemuSnapshot::is_empty")]
    pub qemu: ArceosQemuSnapshot,
    #[serde(default, skip_serializing_if = "ArceosUbootSnapshot::is_empty")]
    pub uboot: ArceosUbootSnapshot,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedBuildRequest {
    pub package: String,
    pub arch: String,
    pub target: String,
    pub smp: Option<usize>,
    pub debug: bool,
    pub source: ConfigSource,
    pub config: ArceosConfig,
    pub runtime: RuntimeValues,
    pub runtime_env: RuntimeEnv,
    pub environment_features: Vec<String>,
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
    pub preset: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
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

#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedAxvisorRequest {
    pub package: String,
    pub axvisor_dir: PathBuf,
    pub arch: String,
    pub target: String,
    pub smp: Option<usize>,
    pub debug: bool,
    pub source: ConfigSource,
    pub config: AxvisorConfig,
    pub runtime: RuntimeValues,
    pub runtime_env: RuntimeEnv,
    /// Original FEATURES input, reapplied when a test selects another file.
    pub environment_features: Vec<String>,
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
    pub preset: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub smp: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<PathBuf>,
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
    pub smp: Option<usize>,
    pub debug: bool,
    pub source: ConfigSource,
    pub config: StarryConfig,
    pub runtime: RuntimeValues,
    pub runtime_env: RuntimeEnv,
    /// Original FEATURES input, reapplied when a test selects another file.
    pub environment_features: Vec<String>,
    pub qemu_config: Option<PathBuf>,
    pub uboot_config: Option<PathBuf>,
}

impl ResolvedStarryRequest {
    pub(crate) fn config_path(&self) -> Option<&Path> {
        self.source.path()
    }

    pub(crate) fn replace_config_input(
        &mut self,
        input: ConfigInput<StarryConfig>,
    ) -> anyhow::Result<()> {
        input.check_selectors(Some(&self.target), Some(&self.arch))?;
        self.replace_config(input.config)?;
        self.source = input.source;
        Ok(())
    }

    /// Replace a test/build group's input while retaining this request's captured
    /// environment. Explicit runtime CLI values still win over file inputs.
    pub(crate) fn replace_config(&mut self, mut config: StarryConfig) -> anyhow::Result<()> {
        super::resolve_starry_arch_and_target(Some(self.arch.clone()), Some(self.target.clone()))?;
        config.common.target = Some(self.target.clone());
        crate::build::apply_makefile_features(&mut config.common, &self.environment_features)?;
        if let Some(smp) = self.smp {
            config.common.max_cpu_num = Some(smp);
        }
        config.validate()?;
        let runtime = ax_config::resolve_runtime(
            config.common.max_cpu_num,
            config.common.runtime,
            &self.runtime_env,
        )?;
        self.config = config;
        self.runtime = runtime;
        Ok(())
    }
}

impl ResolvedAxvisorRequest {
    pub(crate) fn config_path(&self) -> Option<&Path> {
        self.source.path()
    }

    pub(crate) fn replace_config_input(
        &mut self,
        input: ConfigInput<AxvisorConfig>,
    ) -> anyhow::Result<()> {
        input.check_selectors(Some(&self.target), Some(&self.arch))?;
        self.replace_config(input.config)?;
        self.source = input.source;
        Ok(())
    }

    /// Replace a case's full input using this request's captured environment.
    /// VM selections from the preceding input are never inherited.
    pub(crate) fn replace_config(&mut self, mut config: AxvisorConfig) -> anyhow::Result<()> {
        super::resolve_axvisor_arch_and_target(Some(self.arch.clone()), Some(self.target.clone()))?;
        config.common.target = Some(self.target.clone());
        crate::build::apply_makefile_features(&mut config.common, &self.environment_features)?;
        if let Some(smp) = self.smp {
            config.common.max_cpu_num = Some(smp);
        }
        config.validate()?;
        let runtime = ax_config::resolve_runtime(
            config.common.max_cpu_num,
            config.common.runtime,
            &self.runtime_env,
        )?;
        self.config = config;
        self.runtime = runtime;
        self.vmconfigs.clear();
        Ok(())
    }
}

impl ResolvedBuildRequest {
    pub(crate) fn config_path(&self) -> Option<&Path> {
        self.source.path()
    }
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
