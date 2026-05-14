//! Build system configuration and Cargo integration.
//!
//! This module provides functionality for building operating system projects
//! using Cargo or custom build commands. It supports:
//!
//! - Configuring build options via TOML configuration files
//! - Running pre-build and post-build shell commands
//! - Automatic feature detection and configuration
//! - Multiple runner types (QEMU, U-Boot)
//!
//! # Example
//!
//! ```rust,no_run
//! use ostool::build::config::{BuildConfig, BuildSystem, Cargo};
//! use ostool::Tool;
//!
//! // Build configurations are typically loaded from TOML files
//! // See .build.toml for example configuration format
//! ```

use std::path::Path;

use crate::{
    Tool,
    build::{
        cargo_builder::CargoBuilder,
        config::{Cargo, Custom},
    },
    run::{
        qemu::{QemuConfig, RunQemuOptions},
        uboot::{RunUbootOptions, UbootConfig},
    },
};

/// Cargo builder implementation for building projects.
mod cargo_builder;

/// Build configuration types and structures.
pub mod config;

pub mod someboot;

/// Parameters for running a built Cargo artifact in QEMU.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CargoQemuRunnerArgs {
    /// Optional fully prepared QEMU runtime configuration.
    pub qemu: Option<QemuConfig>,
    /// Whether to enable debug mode (GDB server).
    pub debug: bool,
    /// Whether to dump the device tree blob.
    pub dtb_dump: bool,
    /// Whether to show QEMU output.
    pub show_output: bool,
}

/// Parameters for running a built Cargo artifact on real hardware via U-Boot.
#[derive(Debug, Clone, Default)]
pub struct CargoUbootRunnerArgs {
    /// Optional fully prepared U-Boot runtime configuration.
    pub uboot: Option<UbootConfig>,
    /// Whether to show U-Boot output.
    pub show_output: bool,
}

/// Specifies the type of runner to use after building.
///
/// This enum determines how the built artifact will be executed,
/// either through QEMU emulation or via U-Boot on real hardware.
pub enum CargoRunnerKind {
    /// Run the built artifact in QEMU emulator.
    Qemu(Box<CargoQemuRunnerArgs>),
    /// Run the built artifact on real hardware via U-Boot.
    Uboot(Box<CargoUbootRunnerArgs>),
}

impl CargoRunnerKind {
    pub fn new_qemu(args: CargoQemuRunnerArgs) -> Self {
        Self::Qemu(Box::new(args))
    }

    pub fn new_uboot(args: CargoUbootRunnerArgs) -> Self {
        Self::Uboot(Box::new(args))
    }
}

impl Tool {
    /// Returns the default build configuration template.
    pub fn default_build_config(&self) -> config::BuildConfig {
        config::BuildConfig::default()
    }

    /// Loads a build configuration from a workspace-like directory.
    pub async fn load_build_config_from_dir(
        &mut self,
        dir: &Path,
        menu: bool,
    ) -> anyhow::Result<config::BuildConfig> {
        self.prepare_build_config(Some(dir.join(".build.toml")), menu)
            .await
    }

    /// Loads a build configuration from an explicit file path.
    pub async fn load_build_config_from_path(
        &mut self,
        path: &Path,
        menu: bool,
    ) -> anyhow::Result<config::BuildConfig> {
        self.prepare_build_config(Some(path.to_path_buf()), menu)
            .await
    }

    /// Builds the project using the specified build configuration.
    ///
    /// # Arguments
    ///
    /// * `config` - The build configuration specifying how to build the project.
    ///
    /// # Errors
    ///
    /// Returns an error if the build process fails.
    pub async fn build_with_config(&mut self, config: &config::BuildConfig) -> anyhow::Result<()> {
        match &config.system {
            config::BuildSystem::Custom(custom) => self.build_custom(custom)?,
            config::BuildSystem::Cargo(cargo) => {
                self.cargo_build(cargo).await?;
            }
        }
        Ok(())
    }

    /// Builds the project from the specified configuration file path.
    ///
    /// This is the main entry point for building projects. It loads the
    /// configuration from the specified path (or default `.build.toml`)
    /// and executes the build.
    ///
    /// # Arguments
    ///
    /// * `config_path` - Optional path to the build configuration file.
    ///   Defaults to `.build.toml` in the workspace directory.
    ///
    /// # Errors
    ///
    /// Returns an error if the configuration cannot be loaded or the build fails.
    pub(crate) fn build_custom(&mut self, config: &Custom) -> anyhow::Result<()> {
        self.shell_run_cmd(&config.build_cmd)?;
        Ok(())
    }

    /// Builds the project using Cargo.
    ///
    /// # Arguments
    ///
    /// * `config` - Cargo build configuration.
    ///
    /// # Errors
    ///
    /// Returns an error if the Cargo build fails.
    pub async fn cargo_build(&mut self, config: &Cargo) -> anyhow::Result<()> {
        self.sync_cargo_context(config);
        cargo_builder::CargoBuilder::build_auto(self, config)
            .execute()
            .await
    }

    pub(crate) async fn prepare_runtime_artifacts(
        &mut self,
        config: &config::BuildConfig,
        debug: bool,
    ) -> anyhow::Result<()> {
        match &config.system {
            config::BuildSystem::Custom(custom) => {
                self.prepare_custom_runtime_artifacts(custom).await
            }
            config::BuildSystem::Cargo(cargo) => {
                self.prepare_cargo_runtime_artifacts(cargo, debug).await
            }
        }
    }

    async fn prepare_custom_runtime_artifacts(&mut self, config: &Custom) -> anyhow::Result<()> {
        self.build_custom(config)?;
        self.prepare_elf_artifact(config.elf_path.clone().into(), config.to_bin)
            .await
    }

    async fn prepare_cargo_runtime_artifacts(
        &mut self,
        config: &Cargo,
        debug: bool,
    ) -> anyhow::Result<()> {
        let build_config_path = self.ctx.build_config_path.clone();
        CargoBuilder::build(self, config, build_config_path)
            .debug(debug)
            .skip_objcopy(true)
            .resolve_artifact_from_json(true)
            .execute()
            .await
    }

    /// Builds and runs the project using Cargo with the specified runner.
    ///
    /// # Arguments
    ///
    /// * `config` - Cargo build configuration.
    /// * `runner` - The type of runner to use (QEMU or U-Boot).
    ///
    /// # Errors
    ///
    /// Returns an error if the build or run fails.
    pub async fn cargo_run(
        &mut self,
        config: &Cargo,
        runner: &CargoRunnerKind,
    ) -> anyhow::Result<()> {
        self.sync_cargo_context(config);
        let build_config_path = self.ctx.build_config_path.clone();

        let debug = matches!(runner, CargoRunnerKind::Qemu(args) if args.debug);

        CargoBuilder::build(self, config, build_config_path)
            .debug(debug)
            .skip_objcopy(true)
            .resolve_artifact_from_json(true)
            .execute()
            .await?;

        match runner {
            CargoRunnerKind::Qemu(args) => {
                let qemu = match &args.qemu {
                    Some(config) => config.clone(),
                    None => self.ensure_qemu_config_for_cargo(config).await?,
                };
                self.run_qemu(
                    &qemu,
                    RunQemuOptions {
                        dtb_dump: args.dtb_dump,
                        show_output: args.show_output,
                    },
                )
                .await?;
            }
            CargoRunnerKind::Uboot(args) => {
                let uboot = match &args.uboot {
                    Some(config) => config.clone(),
                    None => self.ensure_uboot_config_for_cargo(config).await?,
                };
                self.run_uboot(
                    &uboot,
                    RunUbootOptions {
                        show_output: args.show_output,
                    },
                )
                .await?;
            }
        }

        Ok(())
    }
}
