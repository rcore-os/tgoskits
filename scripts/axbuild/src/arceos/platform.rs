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
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::arceos::{ArceosConfig, Arch};

/// Platform resolver
pub struct PlatformResolver {
    workspace_root: PathBuf,
}

impl PlatformResolver {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self { workspace_root }
    }

    /// Resolve target triple from architecture
    pub fn resolve_target(arch: &Arch) -> &'static str {
        arch.to_target()
    }

    /// Resolve default platform package for given architecture
    pub fn resolve_default_platform(arch: &Arch) -> String {
        match arch {
            Arch::X86_64 => "axplat-x86-pc".to_string(),
            Arch::AArch64 => "axplat-aarch64-qemu-virt".to_string(),
            Arch::RiscV64 => "axplat-riscv64-qemu-virt".to_string(),
            Arch::LoongArch64 => "axplat-loongarch64-qemu-virt".to_string(),
        }
    }

    /// Resolve default platform name for given architecture
    pub fn resolve_default_platform_name(arch: &Arch) -> String {
        match arch {
            Arch::X86_64 => "x86-pc".to_string(),
            Arch::AArch64 => "aarch64-qemu-virt".to_string(),
            Arch::RiscV64 => "riscv64-qemu-virt".to_string(),
            Arch::LoongArch64 => "loongarch64-qemu-virt".to_string(),
        }
    }

    /// Resolve platform config using cargo-axplat
    pub fn resolve_platform_config(
        &self,
        manifest_dir: &Path,
        platform: Option<&str>,
    ) -> Result<PlatformInfo> {
        let arceos_dir = self.workspace_root.join("os/arceos");
        let target_dir = arceos_dir.join("target");

        // Build cargo-axplat arguments
        let mut cmd = Command::new("cargo");
        cmd.arg("axplat")
            .arg("info")
            .arg("--target-dir")
            .arg(&target_dir)
            .current_dir(manifest_dir);

        if let Some(plat) = platform {
            cmd.arg("--platform").arg(plat);
        }

        let output = cmd.output().context("Failed to run cargo-axplat info")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!(
                "cargo-axplat info failed: {}\nstderr: {}",
                output.status,
                stderr
            );
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        self.parse_axplat_output(&stdout)
    }

    /// Parse cargo-axplat info output
    fn parse_axplat_output(&self, output: &str) -> Result<PlatformInfo> {
        // The output format is JSON-like, we need to parse it
        // For now, return a basic PlatformInfo
        // TODO: Properly parse the cargo-axplat output

        let mut info = PlatformInfo::default();

        for line in output.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            if let Some(rest) = line.strip_prefix("ARCH=") {
                info.arch = rest.trim().to_string();
            } else if let Some(rest) = line.strip_prefix("PLAT_NAME=") {
                info.plat_name = rest.trim().to_string();
            } else if let Some(rest) = line.strip_prefix("PLAT=") {
                info.plat = rest.trim().to_string();
            } else if let Some(rest) = line.strip_prefix("PLAT_FEATURES=") {
                info.plat_features = rest
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
            }
        }

        Ok(info)
    }

    /// Check if platform is dynamic (PLAT_DYN=y)
    pub fn is_dyn_platform(&self, platform: &str) -> bool {
        // Check if platform starts with "myplat" or is marked as dynamic
        platform.starts_with("myplat") || platform.contains("-dyn")
    }
}

/// Platform information
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PlatformInfo {
    /// Architecture
    pub arch: String,

    /// Platform name
    pub plat_name: String,

    /// Platform package
    pub plat: String,

    /// Platform features
    #[serde(default)]
    pub plat_features: Vec<String>,

    /// CPU information
    #[serde(default)]
    pub cpu: CpuInfo,

    /// Memory size (in bytes)
    #[serde(default)]
    pub phys_memory_size: Option<u64>,

    /// Max CPU count
    #[serde(default)]
    pub max_cpu_num: Option<usize>,
}

/// CPU information
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CpuInfo {
    /// CPU vendor
    pub vendor: Option<String>,

    /// CPU model
    pub model: Option<String>,

    /// CPU features
    #[serde(default)]
    pub features: Vec<String>,

    /// Cache info
    #[serde(default)]
    pub cache: CacheInfo,
}

/// Cache information
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CacheInfo {
    /// L1 cache size
    pub l1: Option<usize>,

    /// L2 cache size
    pub l2: Option<usize>,

    /// L3 cache size
    pub l3: Option<usize>,
}

/// Backward-compatible wrapper around the config module's board loader.
pub fn load_board_config(manifest_dir: &Path, board_name: &str) -> Result<ArceosConfig> {
    crate::arceos::config::load_board_config(manifest_dir, board_name)
}
