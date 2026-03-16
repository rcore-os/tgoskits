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
    config::{
        AXCONFIG_FILE_NAME, ArceosConfig, OSTOOL_EXTRA_CONFIG_FILE_NAME, QEMU_CONFIG_FILE_NAME,
        axconfig_path_for_config, qemu_config_path_for_config,
    },
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

#[derive(Debug, Clone)]
pub struct PreparedArtifacts {
    pub cargo_spec: ostool_bridge::CargoBuildSpec,
    pub axconfig_path: PathBuf,
    pub qemu_config_path: PathBuf,
}

/// Builder for ArceOS applications
pub struct Builder {
    config: ArceosConfig,
    manifest_dir: PathBuf,
}

impl Builder {
    pub fn new(config: ArceosConfig, manifest_dir: PathBuf) -> Self {
        Self {
            config,
            manifest_dir,
        }
    }

    pub fn manifest_dir(&self) -> &Path {
        &self.manifest_dir
    }

    /// Execute the build process
    pub async fn build(&self) -> Result<BuildOutput> {
        tracing::info!(
            "Starting build for {:?} in {}",
            self.config.arch,
            self.manifest_dir.display()
        );

        let prepared = prepare_artifacts(&self.manifest_dir, &self.config)?;

        tracing::debug!("ostool cargo config: {:?}", prepared.cargo_spec.cargo);
        let mut ctx = prepared.cargo_spec.ctx.into_app_context();
        ctx.cargo_build(&prepared.cargo_spec.cargo).await?;

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

    /// Clean build artifacts
    pub fn clean(&self) -> Result<()> {
        tracing::info!(
            "Cleaning build artifacts in {}",
            self.manifest_dir.display()
        );

        let status = Command::new("cargo")
            .current_dir(&self.manifest_dir)
            .arg("clean")
            .status()
            .context("Failed to run cargo clean")?;

        if !status.success() {
            anyhow::bail!("cargo clean failed with status: {}", status);
        }

        for file in cleanup_files(&self.manifest_dir, &self.config) {
            if file.exists() {
                std::fs::remove_file(&file)
                    .with_context(|| format!("Failed to remove {}", file.display()))?;
            }
        }

        Ok(())
    }
}

fn cleanup_files(manifest_dir: &Path, config: &ArceosConfig) -> Vec<PathBuf> {
    let app_axconfig = axconfig_path_for_config(manifest_dir, config);
    let legacy_axconfig = manifest_dir.join(AXCONFIG_FILE_NAME);
    let app_qemu_config = qemu_config_path_for_config(manifest_dir, config);
    let legacy_qemu_config = manifest_dir.join(QEMU_CONFIG_FILE_NAME);

    let mut files = vec![
        app_axconfig.clone(),
        app_qemu_config.clone(),
        manifest_dir
            .join(".cargo")
            .join(OSTOOL_EXTRA_CONFIG_FILE_NAME),
    ];
    if app_axconfig != legacy_axconfig {
        files.push(legacy_axconfig);
    }
    if app_qemu_config != legacy_qemu_config {
        files.push(legacy_qemu_config);
    }
    files
}

pub fn prepare_artifacts(manifest_dir: &Path, config: &ArceosConfig) -> Result<PreparedArtifacts> {
    prepare_artifacts_with_qemu_config_path(manifest_dir, config, None)
}

pub fn prepare_artifacts_with_qemu_config_path(
    manifest_dir: &Path,
    config: &ArceosConfig,
    qemu_config_path: Option<PathBuf>,
) -> Result<PreparedArtifacts> {
    let project =
        ArtifactPreparer::new(manifest_dir.to_path_buf(), config.clone(), qemu_config_path);
    project.prepare()
}

struct ArtifactPreparer {
    config: ArceosConfig,
    manifest_dir: PathBuf,
    qemu_config_path: Option<PathBuf>,
}

impl ArtifactPreparer {
    fn new(manifest_dir: PathBuf, config: ArceosConfig, qemu_config_path: Option<PathBuf>) -> Self {
        Self {
            config,
            manifest_dir,
            qemu_config_path,
        }
    }

    fn prepare(&self) -> Result<PreparedArtifacts> {
        let mut config = self.config.clone();
        self.resolve_effective_smp(&mut config)?;
        let plat_dyn = self.resolve_platform(&config)?;
        self.generate_config(&config)?;
        let qemu_config_path = self.qemu_config_path.clone().map_or_else(
            || ostool_bridge::write_qemu_config(&self.manifest_dir, &config),
            Ok,
        )?;

        let ax_features = FeatureResolver::resolve_ax_features(&config, plat_dyn);
        let use_axlibc = self.is_c_app(&config)?;
        let lib_features = FeatureResolver::resolve_lib_features(
            &config,
            if use_axlibc { "axlibc" } else { "axstd" },
        );

        let cargo_spec = ostool_bridge::build_cargo_spec(
            &config,
            &self.manifest_dir,
            &ax_features,
            &lib_features,
            use_axlibc,
            plat_dyn,
        )?;

        Ok(PreparedArtifacts {
            cargo_spec,
            axconfig_path: axconfig_path_for_config(&self.manifest_dir, &config),
            qemu_config_path,
        })
    }

    fn resolve_effective_smp(&self, config: &mut ArceosConfig) -> Result<()> {
        if let Some(smp) = config.smp {
            if smp == 0 {
                anyhow::bail!("invalid SMP value `0`: SMP must be >= 1");
            }
            return Ok(());
        }

        let app_dir = config.app_dir(&self.manifest_dir);
        let platform_package = self.resolve_platform_package(config);
        let plat_config = self.resolve_platform_config_path(&app_dir, &platform_package)?;
        let contents = std::fs::read_to_string(&plat_config)
            .with_context(|| format!("Failed to read {}", plat_config.display()))?;
        let smp = parse_max_cpu_num_from_platform_config(&contents, &plat_config)?;
        config.smp = Some(smp);
        Ok(())
    }

    fn resolve_platform(&self, config: &ArceosConfig) -> Result<bool> {
        let resolver = PlatformResolver::new(self.manifest_dir.clone());
        let plat_dyn = matches!(config.arch, crate::arceos::config::Arch::AArch64)
            || resolver.is_dyn_platform(&config.platform);
        Ok(plat_dyn)
    }

    fn generate_config(&self, config: &ArceosConfig) -> Result<()> {
        let defconfig = self.manifest_dir.join("configs/defconfig.toml");
        let out_config = axconfig_path_for_config(&self.manifest_dir, config);
        let app_dir = config.app_dir(&self.manifest_dir);
        let platform_package = self.resolve_platform_package(config);
        let plat_config = self.resolve_platform_config_path(&app_dir, &platform_package)?;

        let mut args = vec![
            defconfig.display().to_string(),
            plat_config.display().to_string(),
        ];

        args.push("-w".to_string());
        args.push(format!("arch=\"{}\"", config.arch));
        args.push("-w".to_string());
        args.push(format!("platform=\"{}\"", config.platform));

        args.push("-o".to_string());
        args.push(out_config.display().to_string());

        if let Some(mem) = &config.mem {
            let mem = self.parse_mem_size(mem)?;
            args.push("-w".to_string());
            args.push(format!("plat.phys-memory-size={}", mem));
        }

        if let Some(smp) = config.smp {
            args.push("-w".to_string());
            args.push(format!("plat.max-cpu-num={}", smp));
        }

        let status = Command::new("axconfig-gen")
            .current_dir(&self.manifest_dir)
            .args(&args)
            .status()
            .context("Failed to run axconfig-gen")?;

        if !status.success() {
            anyhow::bail!("axconfig-gen failed with status: {}", status);
        }

        Ok(())
    }

    fn resolve_platform_package(&self, config: &ArceosConfig) -> String {
        if config.platform.starts_with("axplat-") {
            config.platform.clone()
        } else {
            PlatformResolver::resolve_default_platform(&config.arch)
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

    fn is_c_app(&self, config: &ArceosConfig) -> Result<bool> {
        let cargo_toml = config.app_dir(&self.manifest_dir).join("Cargo.toml");
        if cargo_toml.exists() {
            let contents = std::fs::read_to_string(&cargo_toml)?;
            return Ok(contents.contains("axlibc"));
        }

        Ok(false)
    }

    fn parse_mem_size(&self, mem: &str) -> Result<String> {
        let script = self.manifest_dir.join("scripts/make/strtosz.py");
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

fn parse_max_cpu_num_from_platform_config(contents: &str, path: &Path) -> Result<usize> {
    let value: toml::Value =
        toml::from_str(contents).with_context(|| format!("Failed to parse {}", path.display()))?;
    let Some(max_cpu_num) = value
        .get("plat")
        .and_then(|plat| plat.get("max-cpu-num"))
        .and_then(|max| max.as_integer())
    else {
        anyhow::bail!(
            "`plat.max-cpu-num` is not defined in the platform configuration file: {}",
            path.display()
        );
    };

    if max_cpu_num < 1 {
        anyhow::bail!(
            "invalid `plat.max-cpu-num` value `{}` in {}: SMP must be >= 1",
            max_cpu_num,
            path.display()
        );
    }

    Ok(max_cpu_num as usize)
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;

    #[test]
    fn cleanup_files_includes_app_local_and_legacy_axconfig_paths() {
        let manifest_dir = PathBuf::from("/workspace/os/arceos");
        let config = ArceosConfig {
            app: PathBuf::from("examples/helloworld"),
            ..ArceosConfig::default()
        };

        let files = cleanup_files(&manifest_dir, &config);
        assert!(files.contains(&manifest_dir.join("examples/helloworld/.axconfig.toml")));
        assert!(files.contains(&manifest_dir.join(".axconfig.toml")));
    }

    #[test]
    fn parse_max_cpu_num_from_platform_config_accepts_positive_value() {
        let path = Path::new("/tmp/axplat.toml");
        let parsed =
            parse_max_cpu_num_from_platform_config("[plat]\nmax-cpu-num = 4\n", path).unwrap();
        assert_eq!(parsed, 4);
    }

    #[test]
    fn parse_max_cpu_num_from_platform_config_rejects_missing_field() {
        let path = Path::new("/tmp/axplat.toml");
        let err = parse_max_cpu_num_from_platform_config("[plat]\nfoo = 1\n", path).unwrap_err();
        assert!(err.to_string().contains("plat.max-cpu-num"));
    }

    #[test]
    fn parse_max_cpu_num_from_platform_config_rejects_zero() {
        let path = Path::new("/tmp/axplat.toml");
        let err =
            parse_max_cpu_num_from_platform_config("[plat]\nmax-cpu-num = 0\n", path).unwrap_err();
        assert!(err.to_string().contains("SMP must be >= 1"));
    }
}
