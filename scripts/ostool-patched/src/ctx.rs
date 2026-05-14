//! Application context and runtime state.
//!
//! This module provides the [`AppContext`] type which stores runtime state
//! and build artifacts produced while ostool is operating.

use std::path::PathBuf;

use object::Architecture;

use crate::build::config::BuildConfig;

/// Build artifacts generated during the build process.
#[derive(Default, Clone, Debug)]
pub struct OutputArtifacts {
    /// Path to the built ELF file.
    pub elf: Option<PathBuf>,
    /// Path to the converted binary file.
    pub bin: Option<PathBuf>,
    /// Cargo-reported directory containing the original ELF artifact.
    pub cargo_artifact_dir: Option<PathBuf>,
    /// Directory containing the runtime artifact consumed by runners.
    pub runtime_artifact_dir: Option<PathBuf>,
}

/// The runtime context holding transient and final execution state.
#[derive(Default, Clone, Debug)]
pub struct AppContext {
    /// Detected CPU architecture from the ELF file.
    pub arch: Option<Architecture>,
    /// Current build configuration.
    pub build_config: Option<BuildConfig>,
    /// Path to the build configuration file.
    pub build_config_path: Option<PathBuf>,
    /// Generated build artifacts.
    pub artifacts: OutputArtifacts,
}
