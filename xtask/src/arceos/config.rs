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
    str::FromStr,
};

use anyhow::{Context, Result};
use axbuild::arceos::{ArceosConfigOverride, Arch, BuildMode, FeatureResolver, parse_qemu_options};

pub fn arceos_manifest_dir() -> Result<PathBuf> {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .context("failed to locate workspace root")?;
    Ok(workspace_root.join("os/arceos"))
}

pub fn build_config_override(
    arch: Option<String>,
    _package: String,
    platform: Option<String>,
    release: bool,
    features: Option<String>,
    smp: Option<usize>,
) -> Result<ArceosConfigOverride> {
    if matches!(smp, Some(0)) {
        anyhow::bail!("invalid SMP value `0`: SMP must be >= 1");
    }
    Ok(ArceosConfigOverride {
        arch: arch
            .as_deref()
            .map(Arch::from_str)
            .transpose()
            .context("failed to parse arch override")?,
        platform,
        mode: release.then_some(BuildMode::Release),
        smp,
        features: features
            .as_deref()
            .map(FeatureResolver::parse_features)
            .map(Some)
            .unwrap_or(None),
        ..Default::default()
    })
}

pub fn run_config_override(
    arch: Option<String>,
    package: String,
    platform: Option<String>,
    release: bool,
    features: Option<String>,
    smp: Option<usize>,
    blk: bool,
    disk_img: Option<String>,
    net: bool,
    net_dev: Option<String>,
    graphic: bool,
    accel: bool,
) -> Result<ArceosConfigOverride> {
    let mut overrides = build_config_override(arch, package, platform, release, features, smp)?;
    overrides.qemu = Some(parse_qemu_options(
        blk, disk_img, net, net_dev, graphic, accel,
    ));
    Ok(overrides)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_manifest_dir_exists() {
        let manifest_dir = arceos_manifest_dir().unwrap();
        assert!(manifest_dir.join("Cargo.toml").exists());
    }

    #[test]
    fn test_build_config_override_resolves_workspace_package() {
        let overrides = build_config_override(
            Some("x86_64".to_string()),
            "arceos-helloworld".to_string(),
            None,
            false,
            Some("fs,net".to_string()),
            Some(4),
        )
        .unwrap();

        assert_eq!(overrides.arch, Some(Arch::X86_64));
        assert_eq!(overrides.smp, Some(4));
        assert_eq!(
            overrides.features,
            Some(vec!["fs".to_string(), "net".to_string()])
        );
    }

    #[test]
    fn test_build_config_override_rejects_zero_smp() {
        let err = build_config_override(
            None,
            "arceos-helloworld".to_string(),
            None,
            false,
            None,
            Some(0),
        )
        .unwrap_err();
        assert!(err.to_string().contains("SMP must be >= 1"));
    }
}
