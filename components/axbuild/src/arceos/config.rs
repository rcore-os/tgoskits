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

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// ArceOS build configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArceosConfig {
    /// Target architecture
    pub arch: Arch,

    /// Platform name
    pub platform: String,

    /// Application path (relative to workspace root)
    pub app: PathBuf,

    /// Build mode
    pub mode: BuildMode,

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

    /// Output directory (optional, default is target/{arch}/release)
    pub output_dir: Option<PathBuf>,
}

impl Default for ArceosConfig {
    fn default() -> Self {
        Self {
            arch: Arch::AArch64,
            platform: "aarch64-qemu-virt".to_string(),
            app: PathBuf::from("examples/helloworld"),
            mode: BuildMode::Debug,
            log: LogLevel::Warn,
            smp: None,
            mem: None,
            features: vec![],
            app_features: vec![],
            qemu: QemuOptions::default(),
            output_dir: None,
        }
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

    /// Get the default link script name
    pub fn to_link_script(self) -> &'static str {
        match self {
            Arch::X86_64 => "linker_x86_64.ld",
            Arch::AArch64 => "linker_aarch64.ld",
            Arch::RiscV64 => "linker_riscv64.ld",
            Arch::LoongArch64 => "linker_loongarch64.ld",
        }
    }

    /// Convert from string
    pub fn from_str(s: &str) -> anyhow::Result<Self> {
        match s.to_lowercase().as_str() {
            "x86_64" | "x86" => Ok(Arch::X86_64),
            "aarch64" | "arm64" => Ok(Arch::AArch64),
            "riscv64" | "riscv" => Ok(Arch::RiscV64),
            "loongarch64" | "loongarch" => Ok(Arch::LoongArch64),
            _ => anyhow::bail!("Unknown architecture: {}", s),
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
        write!(f, "{}", self.to_string())
    }
}

impl BuildMode {
    pub fn to_string(self) -> &'static str {
        match self {
            BuildMode::Release => "release",
            BuildMode::Debug => "debug",
        }
    }

    pub fn from_str(s: &str) -> anyhow::Result<Self> {
        match s.to_lowercase().as_str() {
            "release" | "rel" => Ok(BuildMode::Release),
            "debug" | "dev" => Ok(BuildMode::Debug),
            _ => anyhow::bail!("Unknown build mode: {}", s),
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
    pub fn to_string(self) -> &'static str {
        match self {
            LogLevel::Off => "off",
            LogLevel::Error => "error",
            LogLevel::Warn => "warn",
            LogLevel::Info => "info",
            LogLevel::Debug => "debug",
            LogLevel::Trace => "trace",
        }
    }

    pub fn from_str(s: &str) -> anyhow::Result<Self> {
        match s.to_lowercase().as_str() {
            "off" => Ok(LogLevel::Off),
            "error" => Ok(LogLevel::Error),
            "warn" | "warning" => Ok(LogLevel::Warn),
            "info" => Ok(LogLevel::Info),
            "debug" => Ok(LogLevel::Debug),
            "trace" => Ok(LogLevel::Trace),
            _ => anyhow::bail!("Unknown log level: {}", s),
        }
    }
}

impl std::fmt::Display for LogLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.to_string())
    }
}

/// QEMU options
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
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
        }
    }
}

/// Network device type
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NetDev {
    /// User-mode networking
    User,

    /// TAP device
    Tap,

    /// Bridge device
    Bridge,
}

impl NetDev {
    pub fn from_str(s: &str) -> anyhow::Result<Self> {
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
