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
    collections::HashSet,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use axbuild::arceos::config::{ArceosConfig, Arch, BuildMode, LogLevel, NetDev, QemuOptions};
use cargo_metadata::{MetadataCommand, TargetKind};

/// Load configuration from various sources
///
/// Priority:
/// 1. Command line arguments (provided via args)
/// 2. Board configuration file (if board_name provided)
/// 3. .build.toml in arceos directory
/// 4. Default configuration
pub fn load_config(
    workspace_root: &Path,
    arch: Option<String>,
    package: String,
    platform: Option<String>,
    release: bool,
    features: Option<String>,
) -> Result<ArceosConfig> {
    let mut config = ArceosConfig::default();

    // Try to load from board config first
    let board_config = workspace_root.join("os/arceos/.build.toml");

    if board_config.exists() {
        let contents = std::fs::read_to_string(&board_config)
            .with_context(|| format!("Failed to read {}", board_config.display()))?;

        let board_cfg: ArceosConfig = toml::from_str(&contents)
            .with_context(|| format!("Failed to parse {}", board_config.display()))?;

        // Merge board config
        merge_config(&mut config, board_cfg);
    }

    // Override with command line arguments
    if let Some(arch_str) = arch {
        config.arch = Arch::from_str(&arch_str)?;
    }

    if let Some(platform_name) = platform {
        config.platform = platform_name;
    }

    if release {
        config.mode = BuildMode::Release;
    }

    if let Some(features_str) = features {
        config.features = axbuild::arceos::features::FeatureResolver::parse_features(&features_str);
    }

    // Override app path with workspace package resolution.
    config.app = resolve_package_app_dir(workspace_root, &package)?;

    // Make app path absolute if relative
    if config.app.is_relative() {
        config.app = workspace_root.join("os/arceos").join(&config.app);
    }

    Ok(config)
}

fn resolve_package_app_dir(workspace_root: &Path, package_name: &str) -> Result<PathBuf> {
    let metadata = MetadataCommand::new()
        .manifest_path(workspace_root.join("Cargo.toml"))
        .no_deps()
        .exec()
        .context("failed to load cargo metadata for workspace package resolution")?;

    let workspace_members: HashSet<_> = metadata.workspace_members.iter().cloned().collect();
    let candidates: Vec<_> = metadata
        .packages
        .iter()
        .filter(|pkg| workspace_members.contains(&pkg.id) && pkg.name == package_name)
        .collect();

    if candidates.is_empty() {
        anyhow::bail!(
            "workspace package `{}` not found; only exact workspace package names are supported",
            package_name
        );
    }

    if candidates.len() > 1 {
        let manifest_paths = candidates
            .iter()
            .map(|pkg| pkg.manifest_path.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        anyhow::bail!(
            "found multiple workspace packages named `{}`: {}",
            package_name,
            manifest_paths
        );
    }

    let package = candidates[0];
    let has_bin_target = package
        .targets
        .iter()
        .any(|target| target.kind.iter().any(|kind| kind == &TargetKind::Bin));
    if !has_bin_target {
        anyhow::bail!(
            "workspace package `{}` has no binary target; `arceos run/build -p` only supports \
             runnable packages",
            package_name
        );
    }

    let manifest_path = package.manifest_path.clone().into_std_path_buf();
    let package_dir = manifest_path.parent().with_context(|| {
        format!(
            "failed to resolve package directory from manifest path {}",
            manifest_path.display()
        )
    })?;

    Ok(package_dir.to_path_buf())
}

/// Merge two configs, with `override` taking precedence
fn merge_config(base: &mut ArceosConfig, override_cfg: ArceosConfig) {
    if override_cfg.arch != Arch::default() {
        base.arch = override_cfg.arch;
    }
    if !override_cfg.platform.is_empty() {
        base.platform = override_cfg.platform;
    }
    if override_cfg.app != PathBuf::default() {
        base.app = override_cfg.app;
    }
    if override_cfg.mode != BuildMode::default() {
        base.mode = override_cfg.mode;
    }
    if override_cfg.log != LogLevel::default() {
        base.log = override_cfg.log;
    }
    if override_cfg.smp.is_some() {
        base.smp = override_cfg.smp;
    }
    if override_cfg.mem.is_some() {
        base.mem = override_cfg.mem;
    }
    if !override_cfg.features.is_empty() {
        base.features = override_cfg.features;
    }
    if !override_cfg.app_features.is_empty() {
        base.app_features = override_cfg.app_features;
    }
    if override_cfg.qemu != QemuOptions::default() {
        base.qemu = override_cfg.qemu;
    }
    if override_cfg.output_dir.is_some() {
        base.output_dir = override_cfg.output_dir;
    }
}

/// Parse QEMU options from command line
pub fn parse_qemu_options(
    blk: bool,
    disk_img: Option<String>,
    net: bool,
    net_dev: Option<String>,
    graphic: bool,
    accel: bool,
) -> QemuOptions {
    QemuOptions {
        blk,
        disk_image: disk_img.map(PathBuf::from),
        net,
        net_dev: if let Some(dev) = net_dev {
            NetDev::from_str(&dev).unwrap_or(NetDev::User)
        } else {
            NetDev::User
        },
        graphic,
        accel,
        extra_args: vec![],
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn workspace_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("failed to locate workspace root")
            .to_path_buf()
    }

    #[test]
    fn test_load_config_defaults() {
        let workspace = workspace_root();
        let config = load_config(
            &workspace,
            None,
            "arceos-helloworld".to_string(),
            None,
            false,
            None,
        )
        .unwrap();
        assert_eq!(config.arch, Arch::AArch64);
        assert_eq!(config.platform, "aarch64-qemu-virt");
        assert_eq!(config.mode, BuildMode::Debug);
        assert!(config.app.is_absolute());
        assert!(
            config
                .app
                .ends_with(Path::new("os/arceos/examples/helloworld"))
        );
    }

    #[test]
    fn test_parse_arch() {
        assert_eq!(Arch::from_str("x86_64").unwrap(), Arch::X86_64);
        assert_eq!(Arch::from_str("aarch64").unwrap(), Arch::AArch64);
        assert_eq!(Arch::from_str("riscv64").unwrap(), Arch::RiscV64);
    }

    #[test]
    fn test_load_config_unknown_package() {
        let workspace = workspace_root();
        let err = load_config(
            &workspace,
            None,
            "arceos-nonexistent".to_string(),
            None,
            false,
            None,
        )
        .unwrap_err();
        assert!(
            err.to_string()
                .contains("only exact workspace package names are supported")
        );
    }

    #[test]
    fn test_load_config_rejects_non_binary_package() {
        let workspace = workspace_root();
        let err =
            load_config(&workspace, None, "axhal".to_string(), None, false, None).unwrap_err();
        assert!(err.to_string().contains("has no binary target"));
    }
}
