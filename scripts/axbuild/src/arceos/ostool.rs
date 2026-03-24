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
    fs,
    path::{Path, PathBuf},
};

use ::ostool::{
    Tool, ToolConfig,
    build::{CargoRunnerKind, cargo_builder::CargoBuilder, config::Cargo},
};
use anyhow::{Context, Result};
use cargo_metadata::MetadataCommand;
use serde::Deserialize;

use crate::arceos::{
    FeatureResolver, PlatformResolver,
    config::{AXCONFIG_FILE_NAME, ArceosConfig, Arch},
};

const DEFAULT_AX_IP: &str = "10.0.2.15";
const DEFAULT_AX_GW: &str = "10.0.2.2";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppContextSpec {
    pub manifest: PathBuf,
    pub debug: bool,
}

impl AppContextSpec {
    pub fn into_tool(self) -> Result<Tool> {
        Tool::new(ToolConfig {
            manifest: Some(self.manifest),
            debug: self.debug,
            ..Default::default()
        })
    }
}

#[derive(Debug, Clone)]
pub struct CargoBuildSpec {
    pub cargo: Cargo,
    pub ctx: AppContextSpec,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AxFeaturePrefixFamily {
    AxStd,
    AxFeat,
}

pub fn build_cargo_spec(
    config: &ArceosConfig,
    manifest_dir: &Path,
    app_dir: &Path,
    ax_features: &[String],
    lib_features: &[String],
    use_axlibc: bool,
    plat_dyn: bool,
) -> Result<CargoBuildSpec> {
    let package = package_name(app_dir)?;
    let ax_feature_family = detect_ax_feature_prefix_family(app_dir, &package)?;
    let features = build_features(
        ax_features,
        lib_features,
        &config
            .common
            .features
            .iter()
            .filter(|feat| !FeatureResolver::is_valid_feature(feat))
            .cloned()
            .collect::<Vec<_>>(),
        ax_feature_family,
        use_axlibc,
    );
    let mut cargo_args = build_cargo_args(config, plat_dyn);
    cargo_args.extend(config.common.cargo_args.iter().cloned());

    let cargo = Cargo {
        env: build_env(config, app_dir),
        target: config.common.target.clone(),
        package,
        features,
        log: config.common.log.clone(),
        extra_config: None,
        args: cargo_args.clone(),
        pre_build_cmds: vec![],
        post_build_cmds: vec![],
        to_bin: true,
    };

    let ctx = AppContextSpec {
        manifest: manifest_dir.to_path_buf(),
        debug: !cargo_args.iter().any(|arg| arg == "--release"),
    };

    Ok(CargoBuildSpec { cargo, ctx })
}

fn build_features(
    ax_features: &[String],
    lib_features: &[String],
    app_features: &[String],
    ax_feature_family: AxFeaturePrefixFamily,
    use_axlibc: bool,
) -> Vec<String> {
    let ax_prefix = match ax_feature_family {
        AxFeaturePrefixFamily::AxStd => "axstd/",
        AxFeaturePrefixFamily::AxFeat => "axfeat/",
    };
    let lib_prefix = if use_axlibc { "axlibc/" } else { "axstd/" };

    let mut features =
        Vec::with_capacity(ax_features.len() + lib_features.len() + app_features.len());
    features.extend(ax_features.iter().map(|feat| format!("{ax_prefix}{feat}")));
    features.extend(
        lib_features
            .iter()
            .map(|feat| format!("{lib_prefix}{feat}")),
    );
    features.extend(app_features.iter().cloned());
    features
}

fn detect_ax_feature_prefix_family(app_dir: &Path, package: &str) -> Result<AxFeaturePrefixFamily> {
    let metadata = MetadataCommand::new()
        .current_dir(app_dir)
        .no_deps()
        .exec()
        .with_context(|| {
            format!(
                "failed to load cargo metadata for dependency detection from {}",
                app_dir.display()
            )
        })?;

    let manifest_path = app_dir.join("Cargo.toml");
    let package_info = metadata
        .packages
        .iter()
        .find(|pkg| {
            pkg.name == package && pkg.manifest_path.clone().into_std_path_buf() == manifest_path
        })
        .with_context(|| {
            format!(
                "failed to locate package `{}` from manifest {}",
                package,
                manifest_path.display()
            )
        })?;

    let has_axstd = package_info
        .dependencies
        .iter()
        .any(|dep| dep.name == "axstd" || dep.rename.as_deref() == Some("axstd"));
    let has_axfeat = package_info
        .dependencies
        .iter()
        .any(|dep| dep.name == "axfeat" || dep.rename.as_deref() == Some("axfeat"));

    match (has_axstd, has_axfeat) {
        (true, true) | (true, false) => Ok(AxFeaturePrefixFamily::AxStd),
        (false, true) => Ok(AxFeaturePrefixFamily::AxFeat),
        (false, false) => anyhow::bail!(
            "package `{}` must directly depend on `axstd` or `axfeat`",
            package
        ),
    }
}

fn build_env(config: &ArceosConfig, app_dir: &Path) -> HashMap<String, String> {
    let mut env = HashMap::new();
    let arch = config.arch().unwrap_or(Arch::AArch64);
    env.insert("AX_ARCH".to_string(), arch.to_string());
    env.insert(
        "AX_PLATFORM".to_string(),
        effective_linker_platform_name(config),
    );
    if let Some(log) = &config.common.log {
        env.insert("AX_LOG".to_string(), format!("{:?}", log).to_lowercase());
    }
    env.insert("AX_IP".to_string(), DEFAULT_AX_IP.to_string());
    env.insert("AX_GW".to_string(), DEFAULT_AX_GW.to_string());
    env.insert(
        "AX_CONFIG_PATH".to_string(),
        app_dir.join(AXCONFIG_FILE_NAME).display().to_string(),
    );
    env
}

fn build_cargo_args(config: &ArceosConfig, plat_dyn: bool) -> Vec<String> {
    let arch = config.arch().unwrap_or(Arch::AArch64);
    let mut args = Vec::new();
    args.push("--config".to_string());
    args.push(if plat_dyn {
        format!(
            "target.{}.rustflags=[\"-Clink-arg=-Taxplat.x\"]",
            arch.to_target()
        )
    } else {
        format!(
            "target.{}.rustflags=[\"-Clink-arg=-Tlinker.x\",\"-Clink-arg=-no-pie\",\"\
             -Clink-arg=-znostart-stop-gc\"]",
            arch.to_target()
        )
    });
    args
}

fn effective_linker_platform_name(config: &ArceosConfig) -> String {
    let arch = config.arch().unwrap_or(Arch::AArch64);
    PlatformResolver::resolve_default_platform_name(&arch)
}

pub fn resolve_external_qemu_config_path(
    manifest_dir: &Path,
    config_search_dir: &Path,
    config: &ArceosConfig,
    explicit_qemu_config_path: Option<&Path>,
) -> Result<PathBuf> {
    if let Some(path) = explicit_qemu_config_path {
        if !path.exists() {
            anyhow::bail!("missing qemu config: {}", path.display());
        }
        return Ok(path.to_path_buf());
    }

    let arch = config.arch()?;
    let arch_name = arch.to_qemu_arch();
    let candidates = [
        format!("qemu-{}.toml", arch_name),
        format!(".qemu-{}.toml", arch_name),
        "qemu.toml".to_string(),
        ".qemu.toml".to_string(),
    ];

    for filename in &candidates {
        let path = config_search_dir.join(filename);
        if path.exists() {
            return Ok(path);
        }
    }

    for filename in &candidates {
        let path = manifest_dir.join(filename);
        if path.exists() {
            return Ok(path);
        }
    }

    anyhow::bail!(
        "no external qemu config found for target `{}`; expected one of {:?} under {} or {}",
        config.common.target,
        candidates,
        config_search_dir.display(),
        manifest_dir.display()
    );
}

pub async fn cargo_build(tool: &mut Tool, cargo: &Cargo) -> Result<()> {
    CargoBuilder::build_auto(tool, cargo)
        .resolve_artifact_from_json(true)
        .execute()
        .await
}

pub async fn cargo_run_qemu(tool: &mut Tool, cargo: &Cargo, qemu_config_path: PathBuf) -> Result<()> {
    tool.cargo_run(
        cargo,
        &CargoRunnerKind::Qemu {
            qemu_config: Some(qemu_config_path),
            debug: false,
            dtb_dump: false,
        },
    )
    .await
}

fn package_name(app_dir: &Path) -> Result<String> {
    #[derive(Deserialize)]
    struct Manifest {
        package: Package,
    }

    #[derive(Deserialize)]
    struct Package {
        name: String,
    }

    let manifest_path = app_dir.join("Cargo.toml");
    let manifest = fs::read_to_string(&manifest_path)
        .with_context(|| format!("Failed to read {}", manifest_path.display()))?;
    let manifest: Manifest = toml::from_str(&manifest)
        .with_context(|| format!("Failed to parse {}", manifest_path.display()))?;

    Ok(manifest.package.name)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::arceos::config::ArceosConfig;

    #[test]
    fn resolve_external_qemu_config_path_prefers_search_dir() {
        let dir = tempfile::tempdir().unwrap();
        let manifest_dir = dir.path();
        let search_dir = manifest_dir.join("search");
        fs::create_dir_all(&search_dir).unwrap();
        let config_path = search_dir.join("qemu-aarch64.toml");
        fs::write(&config_path, "args = []\nuefi = false\nto_bin = true\n").unwrap();

        let config = ArceosConfig::default();
        let resolved =
            resolve_external_qemu_config_path(manifest_dir, &search_dir, &config, None).unwrap();
        assert_eq!(resolved, config_path);
    }

    #[test]
    fn resolve_external_qemu_config_path_requires_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let manifest_dir = dir.path();
        let search_dir = manifest_dir.join("search");
        fs::create_dir_all(&search_dir).unwrap();
        let explicit = PathBuf::from("/tmp/not-existing-qemu.toml");

        let err = resolve_external_qemu_config_path(
            manifest_dir,
            &search_dir,
            &ArceosConfig::default(),
            Some(explicit.as_path()),
        )
        .unwrap_err();

        assert!(err.to_string().contains("missing qemu config"));
    }
}
