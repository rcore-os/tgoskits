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
    collections::HashMap,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result};

use crate::arceos::{
    config::{ArceosConfig, Arch},
    features::FeatureResolver,
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
        let lib_features = if use_axlibc {
            FeatureResolver::resolve_lib_features(&self.config, "axlibc")
        } else {
            vec![]
        };
        tracing::debug!("lib_features: {:?}", lib_features);

        // 5. Execute cargo build
        let elf_path = self
            .cargo_build(&ax_features, &lib_features, use_axlibc)
            .await?;

        // 7. Convert to bin/uimg
        let final_image = self.convert_image(&elf_path)?;

        tracing::info!("Build completed successfully");
        Ok(BuildOutput {
            elf: elf_path,
            bin: final_image,
        })
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

    /// Execute cargo build
    async fn cargo_build(
        &self,
        ax_features: &[String],
        lib_features: &[String],
        use_axlibc: bool,
    ) -> Result<PathBuf> {
        let target = self.config.arch.to_target();
        let mode = self.config.mode.to_string();
        let plat_dyn = ax_features.iter().any(|feat| feat == "plat-dyn");
        let target_dir = self.target_dir.join(target).join(mode);

        // Build cargo arguments
        let target_dir_str = self.target_dir.to_string_lossy().to_string();
        let mut args: Vec<String> = vec![
            "build".to_string(),
            "-Z".to_string(),
            "unstable-options".to_string(),
            "--target".to_string(),
            target.to_string(),
            "--target-dir".to_string(),
            target_dir_str,
        ];

        if plat_dyn {
            args.push("-Z".to_string());
            args.push("build-std=core,alloc".to_string());
        }

        if self.config.mode == crate::arceos::config::BuildMode::Release {
            args.push("--release".to_string());
        }

        // Combine all features following ArceOS Makefile's feature-prefix strategy.
        let mut all_features: Vec<String> = Vec::new();
        let ax_feat_prefix = if use_axlibc { "axfeat/" } else { "axstd/" };
        let lib_feat_prefix = if use_axlibc { "axlibc/" } else { "axstd/" };

        all_features.extend(
            ax_features
                .iter()
                .map(|feat| format!("{ax_feat_prefix}{feat}")),
        );
        all_features.extend(
            lib_features
                .iter()
                .map(|feat| format!("{lib_feat_prefix}{feat}")),
        );
        all_features.extend(self.config.app_features.iter().cloned());

        if !all_features.is_empty() {
            args.push("--features".to_string());
            args.push(all_features.join(","));
        }

        // Set environment variables
        let mut env = std::env::vars().collect::<HashMap<_, _>>();

        // Set RUSTFLAGS
        let mut rustflags = vec!["-A".to_string(), "unsafe_op_in_unsafe_fn".to_string()];
        let link_script = self
            .target_dir
            .join(target)
            .join(mode)
            .join(format!("linker_{}.lds", self.config.platform));

        if self.is_c_app()? {
            // C app uses axlibc
            let axlibc_linker = self
                .target_dir
                .join("axlibc")
                .join(target)
                .join("release")
                .join("axlibc.a");
            if axlibc_linker.exists() {
                rustflags.push(format!("-Clink-arg={}", axlibc_linker.display()));
            }
        } else if plat_dyn {
            rustflags.push("-Crelocation-model=pic".to_string());
            rustflags.push("-Clink-arg=-pie".to_string());
            rustflags.push("-Clink-arg=-znostart-stop-gc".to_string());
            rustflags.push("-Clink-arg=-Taxplat.x".to_string());
        } else {
            // Rust app
            rustflags.push(format!("-Clink-arg=-T{}", link_script.display()));
            rustflags.push("-Clink-arg=-no-pie".to_string());
            rustflags.push("-Clink-arg=-znostart-stop-gc".to_string());
        }

        env.insert(
            "RUSTFLAGS".to_string(),
            std::env::var("RUSTFLAGS")
                .map(|v| format!("{} {}", v, rustflags.join(" ")))
                .unwrap_or_else(|_| rustflags.join(" ")),
        );

        env.insert("AX_ARCH".to_string(), self.config.arch.to_string());
        env.insert("AX_PLATFORM".to_string(), self.config.platform.clone());
        env.insert(
            "AX_LOG".to_string(),
            self.config.log.to_string().to_string(),
        );
        env.insert(
            "AX_CONFIG_PATH".to_string(),
            self.arceos_dir.join(".axconfig.toml").display().to_string(),
        );

        let app_dir = self.app_dir();

        tracing::info!("Running cargo build in {}", app_dir.display());
        tracing::debug!("Cargo args: {:?}", args);

        // For async, we use tokio's process
        #[cfg(feature = "tokio")]
        {
            let output = tokio::process::Command::new("cargo")
                .current_dir(&app_dir)
                .args(&args)
                .envs(&env)
                .output()
                .await
                .context("Failed to run cargo build")?;

            if !output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);
                anyhow::bail!(
                    "Cargo build failed\nstdout:\n{}\nstderr:\n{}",
                    stdout,
                    stderr
                );
            }
        }

        #[cfg(not(feature = "tokio"))]
        {
            let output = Command::new("cargo")
                .current_dir(&app_dir)
                .args(&args)
                .envs(&env)
                .output()
                .context("Failed to run cargo build")?;

            if !output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);
                anyhow::bail!(
                    "Cargo build failed\nstdout:\n{}\nstderr:\n{}",
                    stdout,
                    stderr
                );
            }
        }

        // Find the output ELF file
        let elf_name = self.get_output_name()?;
        let elf_path = target_dir.join(&elf_name);

        if !elf_path.exists() {
            anyhow::bail!("Built ELF file not found at {}", elf_path.display());
        }

        Ok(elf_path)
    }

    /// Get the output binary name
    fn get_output_name(&self) -> Result<String> {
        let app_dir = self.app_dir();

        // Read Cargo.toml to get package name
        let cargo_toml = app_dir.join("Cargo.toml");
        if cargo_toml.exists() {
            let contents = std::fs::read_to_string(&cargo_toml)?;
            for line in contents.lines() {
                if line.trim().starts_with("name = ") {
                    let name = line
                        .trim()
                        .strip_prefix("name = ")
                        .and_then(|s| s.strip_prefix('"'))
                        .and_then(|s| s.strip_suffix('"'))
                        .unwrap_or("arceos_app");
                    return Ok(name.to_string());
                }
            }
        }

        Ok("arceos_app".to_string())
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

    /// Convert ELF to binary/uimg
    fn convert_image(&self, elf_path: &Path) -> Result<PathBuf> {
        let file_name = elf_path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("output");
        let bin_name = if let Some(stem) = file_name.strip_suffix(".elf") {
            format!("{stem}.bin")
        } else {
            format!("{file_name}.bin")
        };

        let bin_path = elf_path.parent().unwrap().join(&bin_name);

        // Use rust-objcopy to convert ELF to binary
        let status = Command::new("rust-objcopy")
            .arg("-O")
            .arg("binary")
            .arg(elf_path)
            .arg(&bin_path)
            .status()
            .context("Failed to run rust-objcopy")?;

        if !status.success() {
            anyhow::bail!("rust-objcopy failed with status: {}", status);
        }

        tracing::debug!("Converted {} to {}", elf_path.display(), bin_path.display());

        Ok(bin_path)
    }

    /// Clean build artifacts
    pub fn clean(&self) -> Result<()> {
        tracing::info!("Cleaning build artifacts");

        let app_dir = self.app_dir();

        // Clean the app directory
        let status = Command::new("cargo")
            .current_dir(&app_dir)
            .args(&["clean"])
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
