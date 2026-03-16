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

use crate::arceos::{
    config::{ArceosConfig, Arch},
    features::FeatureResolver,
    ostool as ostool_bridge,
    platform::PlatformResolver,
};

/// Build output
#[derive(Debug, Clone)]
pub struct BuildOutput {
    /// Path to the ELF file
    pub elf: PathBuf,
    /// Path to the final binary/image file
    pub bin: PathBuf,
}

/// Builder for ArceOS applications
pub struct Builder {
    config: ArceosConfig,
    workspace_root: PathBuf,
    arceos_dir: PathBuf,
    target_dir: PathBuf,
}

impl Builder {
    pub fn new(config: ArceosConfig, workspace_root: PathBuf) -> Self {
        let arceos_dir = workspace_root.join("os/arceos");
        let target_dir = arceos_dir.join("target");

        Self {
            config,
            workspace_root,
            arceos_dir,
            target_dir,
        }
    }

    /// Execute the build process
    pub async fn build(&self) -> Result<BuildOutput> {
        tracing::info!("Starting build for {:?}", self.config.arch);

        // 1. Resolve platform
        tracing::info!("Resolving platform: {}", self.config.platform);
        let plat_dyn = self.resolve_platform()?;

        // 2. Generate configuration
        self.generate_config()?;

        // 3. Resolve features
        let ax_features = FeatureResolver::resolve_ax_features(&self.config, plat_dyn);
        tracing::debug!("ax_features: {:?}", ax_features);

        // 4. Determine if using axlibc (C app)
        let use_axlibc = self.is_c_app()?;
        let lib_features = FeatureResolver::resolve_lib_features(
            &self.config,
            if use_axlibc { "axlibc" } else { "axstd" },
        );
        tracing::debug!("lib_features: {:?}", lib_features);

        // 5. Execute cargo build
        let output = self
            .build_with_ostool(&ax_features, &lib_features, use_axlibc, plat_dyn)
            .await?;

        tracing::info!("Build completed successfully");
        Ok(output)
    }

    /// Resolve platform and check if it's dynamic
    fn resolve_platform(&self) -> Result<bool> {
        let resolver = PlatformResolver::new(self.workspace_root.clone());
        let plat_dyn = matches!(self.config.arch, Arch::AArch64)
            || resolver.is_dyn_platform(&self.config.platform);
        Ok(plat_dyn)
    }

    /// Generate axconfig.toml using axconfig-gen
    fn generate_config(&self) -> Result<()> {
        let arceos_dir = &self.arceos_dir;
        let defconfig = arceos_dir.join("configs/defconfig.toml");
        let out_config = arceos_dir.join(".axconfig.toml");
        let app_dir = self.app_dir();
        let platform_package = self.resolve_platform_package();
        let plat_config = self.resolve_platform_config_path(&app_dir, &platform_package)?;

        let mut args = vec![
            defconfig.display().to_string(),
            plat_config.display().to_string(),
        ];

        // Set variables
        args.push("-w".to_string());
        args.push(format!("arch=\"{}\"", self.config.arch));
        args.push("-w".to_string());
        args.push(format!("platform=\"{}\"", self.config.platform));

        // Output
        args.push("-o".to_string());
        args.push(out_config.display().to_string());

        // Memory size
        if let Some(mem) = &self.config.mem {
            let mem = self.parse_mem_size(mem)?;
            args.push("-w".to_string());
            args.push(format!("plat.phys-memory-size={}", mem));
        }

        // SMP
        if let Some(smp) = self.config.smp {
            args.push("-w".to_string());
            args.push(format!("plat.max-cpu-num={}", smp));
        }

        tracing::debug!("Running axconfig-gen with args: {:?}", args);

        let status = Command::new("axconfig-gen")
            .current_dir(arceos_dir)
            .args(&args)
            .status()
            .context("Failed to run axconfig-gen")?;

        if !status.success() {
            anyhow::bail!("axconfig-gen failed with status: {}", status);
        }

        tracing::debug!("Generated config at {}", out_config.display());
        Ok(())
    }

    fn resolve_platform_package(&self) -> String {
        if self.config.platform.starts_with("axplat-") {
            self.config.platform.clone()
        } else {
            PlatformResolver::resolve_default_platform(&self.config.arch)
        }
    }

    fn resolve_platform_config_path(
        &self,
        app_dir: &Path,
        platform_package: &str,
    ) -> Result<PathBuf> {
        let output = Command::new("cargo")
            .arg("axplat")
            .arg("info")
            .arg("-C")
            .arg(app_dir)
            .arg("-c")
            .arg(platform_package)
            .output()
            .with_context(|| {
                format!(
                    "Failed to run `cargo axplat info -C {} -c {}`",
                    app_dir.display(),
                    platform_package
                )
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!(
                "cargo axplat info failed for package `{}`: {}\nstderr:\n{}",
                platform_package,
                output.status,
                stderr
            );
        }

        let config_path = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if config_path.is_empty() {
            anyhow::bail!(
                "cargo axplat info returned empty config path for package `{}`",
                platform_package
            );
        }

        let path = PathBuf::from(config_path);
        if !path.exists() {
            anyhow::bail!("platform config path does not exist: {}", path.display());
        }

        Ok(path)
    }

    async fn build_with_ostool(
        &self,
        ax_features: &[String],
        lib_features: &[String],
        use_axlibc: bool,
        plat_dyn: bool,
    ) -> Result<BuildOutput> {
        let spec = ostool_bridge::build_cargo_spec(
            &self.config,
            &self.workspace_root,
            &self.arceos_dir,
            &self.target_dir,
            ax_features,
            lib_features,
            use_axlibc,
            plat_dyn,
        )?;

        tracing::info!("Running cargo build in {}", spec.ctx.manifest.display());
        tracing::debug!("ostool cargo config: {:?}", spec.cargo);

        let mut ctx = spec.ctx.into_app_context();
        let workspace_root = ctx.paths.workspace.clone();
        // Keep ArceOS build semantics owned by axbuild instead of someboot auto-detection.
        ctx.paths.workspace = workspace_root.join(".axbuild-ostool");
        let build_result = ctx.cargo_build(&spec.cargo).await;
        ctx.paths.workspace = workspace_root;
        build_result?;

        let elf = ctx
            .paths
            .artifacts
            .elf
            .clone()
            .context("ostool build did not produce an ELF artifact")?;
        let bin = ctx
            .paths
            .artifacts
            .bin
            .clone()
            .context("ostool build did not produce a BIN artifact")?;

        Ok(BuildOutput { elf, bin })
    }

    /// Check if the app is a C application (using axlibc)
    fn is_c_app(&self) -> Result<bool> {
        let app_dir = self.app_dir();

        let cargo_toml = app_dir.join("Cargo.toml");
        if cargo_toml.exists() {
            let contents = std::fs::read_to_string(&cargo_toml)?;
            // Check if it depends on axlibc
            return Ok(contents.contains("axlibc"));
        }

        Ok(false)
    }

    /// Clean build artifacts
    pub fn clean(&self) -> Result<()> {
        tracing::info!("Cleaning build artifacts");

        let app_dir = self.app_dir();

        // Clean the app directory
        let status = Command::new("cargo")
            .current_dir(&app_dir)
            .arg("clean")
            .arg("--target-dir")
            .arg(&self.target_dir)
            .status()
            .context("Failed to run cargo clean")?;

        if !status.success() {
            anyhow::bail!("cargo clean failed with status: {}", status);
        }

        // Clean the output config
        let out_config = self.arceos_dir.join(".axconfig.toml");
        if out_config.exists() {
            std::fs::remove_file(&out_config)
                .with_context(|| format!("Failed to remove {}", out_config.display()))?;
        }

        let qemu_config = self.arceos_dir.join(".qemu.toml");
        if qemu_config.exists() {
            std::fs::remove_file(&qemu_config)
                .with_context(|| format!("Failed to remove {}", qemu_config.display()))?;
        }

        Ok(())
    }

    fn app_dir(&self) -> PathBuf {
        if self.config.app.is_absolute() {
            self.config.app.clone()
        } else {
            self.arceos_dir.join(&self.config.app)
        }
    }

    fn parse_mem_size(&self, mem: &str) -> Result<String> {
        let script = self.arceos_dir.join("scripts/make/strtosz.py");
        let output = Command::new(&script)
            .arg(mem)
            .output()
            .with_context(|| format!("Failed to run {}", script.display()))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!(
                "failed to parse memory size `{}` via {}: {}",
                mem,
                script.display(),
                stderr.trim()
            );
        }

        let parsed = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if parsed.is_empty() {
            anyhow::bail!("failed to parse memory size `{}`: empty output", mem);
        }
        Ok(parsed)
    }
}
