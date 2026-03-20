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
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result};

use crate::arceos::{
    config::{AXCONFIG_FILE_NAME, ArceosConfig},
    context::AxContext,
    features::FeatureResolver,
    ostool as ostool_bridge,
    platform::PlatformResolver,
};

const DEFAULT_DEFCONFIG_CONTENT: &str = r#"# Stack size of each task.
task-stack-size = 0x40000   # uint

# Number of timer ticks per second (Hz). A timer tick may contain several timer
# interrupts.
ticks-per-sec = 100         # uint
"#;

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
}

/// Builder for ArceOS applications
pub struct Builder {
    ctx: AxContext,
}

impl Builder {
    pub fn new(ctx: AxContext) -> Self {
        Self { ctx }
    }

    /// Execute the build process
    pub async fn build(&self) -> Result<BuildOutput> {
        tracing::info!(
            "Starting build for {:?} in {}",
            self.ctx.config.arch,
            self.ctx.manifest_dir().display()
        );

        let prepared = prepare_artifacts(
            self.ctx.manifest_dir(),
            self.ctx.app_dir(),
            &self.ctx.config,
        )?;

        tracing::debug!("ostool cargo config: {:?}", prepared.cargo_spec.cargo);
        let mut ctx = prepared.cargo_spec.ctx.into_app_context();
        ostool_bridge::cargo_build(&mut ctx, &prepared.cargo_spec.cargo).await?;

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
            self.ctx.manifest_dir().display()
        );

        let status = Command::new("cargo")
            .current_dir(self.ctx.manifest_dir())
            .arg("clean")
            .status()
            .context("Failed to run cargo clean")?;

        if !status.success() {
            anyhow::bail!("cargo clean failed with status: {}", status);
        }

        for file in cleanup_files(self.ctx.manifest_dir(), self.ctx.app_dir()) {
            if file.exists() {
                std::fs::remove_file(&file)
                    .with_context(|| format!("Failed to remove {}", file.display()))?;
            }
        }

        Ok(())
    }
}

fn cleanup_files(_manifest_dir: &Path, app_dir: &Path) -> Vec<PathBuf> {
    vec![app_dir.join(AXCONFIG_FILE_NAME)]
}

pub fn prepare_artifacts(
    manifest_dir: &Path,
    app_dir: &Path,
    config: &ArceosConfig,
) -> Result<PreparedArtifacts> {
    let project = ArtifactPreparer::new(
        manifest_dir.to_path_buf(),
        app_dir.to_path_buf(),
        config.clone(),
    );
    project.prepare()
}

pub(crate) fn resolve_effective_config(
    manifest_dir: &Path,
    app_dir: &Path,
    config: &ArceosConfig,
) -> Result<ArceosConfig> {
    ArtifactPreparer::new(
        manifest_dir.to_path_buf(),
        app_dir.to_path_buf(),
        config.clone(),
    )
    .effective_config()
}

struct ArtifactPreparer {
    config: ArceosConfig,
    manifest_dir: PathBuf,
    app_dir: PathBuf,
}

impl ArtifactPreparer {
    fn new(manifest_dir: PathBuf, app_dir: PathBuf, config: ArceosConfig) -> Self {
        Self {
            config,
            manifest_dir,
            app_dir,
        }
    }

    fn effective_config(&self) -> Result<ArceosConfig> {
        let mut config = self.config.clone();
        self.resolve_effective_smp(&mut config)?;
        Ok(config)
    }

    fn prepare(&self) -> Result<PreparedArtifacts> {
        let config = self.effective_config()?;
        let plat_dyn = self.resolve_platform(&config)?;
        self.generate_config(&config)?;

        let ax_features = FeatureResolver::resolve_ax_features(&config, plat_dyn);
        let use_axlibc = self.is_c_app()?;
        let lib_features = FeatureResolver::resolve_lib_features(
            &config,
            if use_axlibc { "axlibc" } else { "axstd" },
        );

        let cargo_spec = ostool_bridge::build_cargo_spec(
            &config,
            &self.manifest_dir,
            &self.app_dir,
            &ax_features,
            &lib_features,
            use_axlibc,
            plat_dyn,
        )?;

        Ok(PreparedArtifacts {
            cargo_spec,
            axconfig_path: self.app_dir.join(AXCONFIG_FILE_NAME),
        })
    }

    fn resolve_effective_smp(&self, config: &mut ArceosConfig) -> Result<()> {
        if let Some(smp) = config.smp {
            if smp == 0 {
                anyhow::bail!("invalid SMP value `0`: SMP must be >= 1");
            }
            return Ok(());
        }

        let platform_package = self.resolve_platform_package(config);
        let plat_config = self.resolve_platform_config_path(&self.app_dir, &platform_package)?;
        let contents = std::fs::read_to_string(&plat_config)
            .with_context(|| format!("Failed to read {}", plat_config.display()))?;
        let smp = parse_max_cpu_num_from_platform_config(&contents, &plat_config)?;
        config.smp = Some(smp);
        Ok(())
    }

    fn resolve_platform(&self, config: &ArceosConfig) -> Result<bool> {
        if let Some(plat_dyn) = config.plat_dyn {
            return Ok(plat_dyn);
        }
        let resolver = PlatformResolver::new(self.manifest_dir.clone());
        let plat_dyn = matches!(config.arch, crate::arceos::config::Arch::AArch64)
            || resolver.is_dyn_platform(&config.platform);
        Ok(plat_dyn)
    }

    fn generate_config(&self, config: &ArceosConfig) -> Result<()> {
        let defconfig = resolve_defconfig_path(&self.manifest_dir, &workspace_root_path()?)?;
        let out_config = self.app_dir.join(AXCONFIG_FILE_NAME);
        let platform_package = self.resolve_platform_package(config);
        let plat_config = self.resolve_platform_config_path(&self.app_dir, &platform_package)?;

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

    fn is_c_app(&self) -> Result<bool> {
        let cargo_toml = self.app_dir.join("Cargo.toml");
        if cargo_toml.exists() {
            let contents = std::fs::read_to_string(&cargo_toml)?;
            return Ok(contents.contains("axlibc"));
        }

        Ok(false)
    }

    fn parse_mem_size(&self, mem: &str) -> Result<String> {
        let script = resolve_strtosz_script_path(&self.manifest_dir)?;
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

fn workspace_root_path() -> Result<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .context("failed to locate workspace root from axbuild crate")?;
    Ok(root.to_path_buf())
}

fn resolve_defconfig_path(manifest_dir: &Path, workspace_root: &Path) -> Result<PathBuf> {
    let manifest_defconfig = manifest_dir.join("configs/defconfig.toml");
    if manifest_defconfig.exists() {
        return Ok(manifest_defconfig);
    }

    let fallback = workspace_root.join("target/defconfig.toml");
    ensure_default_defconfig_file(&fallback)?;
    Ok(fallback)
}

fn ensure_default_defconfig_file(path: &Path) -> Result<()> {
    if path.exists() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    fs::write(path, DEFAULT_DEFCONFIG_CONTENT)
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

fn resolve_strtosz_script_path(manifest_dir: &Path) -> Result<PathBuf> {
    let candidates = [
        manifest_dir.join("scripts/make/strtosz.py"),
        manifest_dir.join("make/strtosz.py"),
    ];
    for path in candidates {
        if path.exists() {
            return Ok(path);
        }
    }

    bail!(
        "strtosz.py not found under `{}` (checked scripts/make and make)",
        manifest_dir.display()
    )
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

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn cleanup_files_includes_app_local_config_paths() {
        let manifest_dir = PathBuf::from("/workspace/os/arceos");
        let app_dir = PathBuf::from("examples/helloworld");

        let files = cleanup_files(&manifest_dir, &app_dir);
        assert!(files.contains(&app_dir.join(AXCONFIG_FILE_NAME)));
        assert_eq!(files.len(), 1);
    }

    #[test]
    fn cleanup_files_returns_correct_paths_for_root_app() {
        let manifest_dir = PathBuf::from("/workspace/os/arceos");
        let app_dir = PathBuf::from(".");

        let files = cleanup_files(&manifest_dir, &app_dir);
        assert!(files.contains(&app_dir.join(AXCONFIG_FILE_NAME)));
        assert_eq!(files.len(), 1);
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

    #[test]
    fn resolve_defconfig_path_prefers_manifest_configs() {
        let dir = tempdir().unwrap();
        let manifest = dir.path().join("manifest");
        let workspace = dir.path().join("workspace");
        fs::create_dir_all(manifest.join("configs")).unwrap();
        fs::create_dir_all(&workspace).unwrap();
        fs::write(manifest.join("configs/defconfig.toml"), "key = 1\n").unwrap();

        let resolved = resolve_defconfig_path(&manifest, &workspace).unwrap();
        assert_eq!(resolved, manifest.join("configs/defconfig.toml"));
    }

    #[test]
    fn resolve_defconfig_path_falls_back_to_workspace_target() {
        let dir = tempdir().unwrap();
        let manifest = dir.path().join("manifest");
        let workspace = dir.path().join("workspace");
        fs::create_dir_all(&manifest).unwrap();
        fs::create_dir_all(&workspace).unwrap();

        let resolved = resolve_defconfig_path(&manifest, &workspace).unwrap();
        let expected = workspace.join("target/defconfig.toml");
        assert_eq!(resolved, expected);
        let contents = fs::read_to_string(expected).unwrap();
        assert_eq!(contents, DEFAULT_DEFCONFIG_CONTENT);
    }

    #[test]
    fn resolve_strtosz_script_path_accepts_make_fallback() {
        let dir = tempdir().unwrap();
        let manifest = dir.path().join("manifest");
        fs::create_dir_all(manifest.join("make")).unwrap();
        fs::write(manifest.join("make/strtosz.py"), "#!/usr/bin/env python3\n").unwrap();

        let resolved = resolve_strtosz_script_path(&manifest).unwrap();
        assert_eq!(resolved, manifest.join("make/strtosz.py"));
    }
}
