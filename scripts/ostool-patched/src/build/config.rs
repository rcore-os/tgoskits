//! Build configuration types and structures.
//!
//! This module defines the configuration structures used to specify how
//! operating system projects should be built. Configuration is typically
//! stored in `.build.toml` files.
//!
//! # Configuration File Format
//!
//! ```toml
//! [system.Cargo]
//! target = "aarch64-unknown-none"
//! package = "my-kernel"
//! features = ["feature1", "feature2"]
//! to_bin = true
//! ```

use std::collections::HashMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Root build configuration structure.
///
/// This is the top-level configuration that specifies which build system
/// to use (Cargo or custom shell commands).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct BuildConfig {
    /// The build system configuration.
    pub system: BuildSystem,
}

impl Default for BuildConfig {
    fn default() -> Self {
        Self {
            system: BuildSystem::Cargo(Cargo::default()),
        }
    }
}

/// Specifies the build system to use.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub enum BuildSystem {
    /// Use custom shell commands for building.
    Custom(Custom),
    /// Use Cargo for building.
    Cargo(Cargo),
}

/// Configuration for custom (non-Cargo) build systems.
///
/// This allows using arbitrary shell commands for building,
/// useful for projects that don't use Cargo or need special build steps.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct Custom {
    /// Shell command to build the kernel.
    pub build_cmd: String,
    /// Path to the built ELF file.
    pub elf_path: String,
    /// Whether to convert the ELF to raw binary format.
    pub to_bin: bool,
}

/// Configuration for Cargo-based builds.
///
/// This structure contains all the options needed to configure a Cargo build,
/// including target architecture, features, environment variables, and build hooks.
#[derive(Default, Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct Cargo {
    /// Environment variables to set during the build.
    pub env: HashMap<String, String>,
    /// Target triple (e.g., "aarch64-unknown-none", "riscv64gc-unknown-none-elf").
    pub target: String,
    /// Package name to build.
    pub package: String,
    /// Cargo features to enable.
    pub features: Vec<String>,
    /// Log level feature to automatically enable.
    pub log: Option<LogLevel>,
    /// Extra Cargo config file path or URL.
    ///
    /// Can be a local path or a URL (including GitHub URLs which are
    /// automatically converted to raw content URLs).
    pub extra_config: Option<String>,
    /// Additional Cargo command-line arguments.
    pub args: Vec<String>,
    /// Shell commands to run before the build.
    pub pre_build_cmds: Vec<String>,
    /// Shell commands to run after the build.
    ///
    /// The `KERNEL_ELF` environment variable is set to the built ELF path.
    pub post_build_cmds: Vec<String>,
    /// Whether to convert the ELF to raw binary format after building.
    pub to_bin: bool,
}

/// Dependency configuration for feature management.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct Depend {
    /// Dependency name.
    pub name: String,
    /// Features to enable for this dependency.
    pub d_features: Vec<String>,
}

/// Log level configuration for the `log` crate.
///
/// When specified, automatically enables the corresponding
/// `log/max_level_*` or `log/release_max_level_*` feature.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub enum LogLevel {
    /// Trace level logging.
    Trace,
    /// Debug level logging.
    Debug,
    /// Info level logging.
    Info,
    /// Warning level logging.
    Warn,
    /// Error level logging.
    Error,
}
