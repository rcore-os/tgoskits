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
    build::config::Cargo,
    ctx::{AppContext, OutputConfig, PathConfig},
    run::qemu::QemuConfig,
};
use anyhow::{Context, Result};
use cargo_metadata::MetadataCommand;
use serde::Deserialize;

use crate::arceos::{
    PlatformResolver,
    config::{AXCONFIG_FILE_NAME, ArceosConfig, Arch, BuildMode, NetDev},
};

const DEFAULT_AX_IP: &str = "10.0.2.15";
const DEFAULT_AX_GW: &str = "10.0.2.2";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppContextSpec {
    pub workspace: PathBuf,
    pub manifest: PathBuf,
    pub debug: bool,
}

impl AppContextSpec {
    pub fn into_app_context(self) -> AppContext {
        AppContext {
            paths: PathConfig {
                workspace: self.workspace,
                manifest: self.manifest,
                config: OutputConfig::default(),
                ..Default::default()
            },
            debug: self.debug,
            ..Default::default()
        }
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
        &config.app_features,
        ax_feature_family,
        use_axlibc,
    );

    let cargo = Cargo {
        env: build_env(config, app_dir),
        target: config.arch.to_target().to_string(),
        package,
        features,
        log: None,
        extra_config: None,
        args: build_cargo_args(config, manifest_dir, plat_dyn),
        pre_build_cmds: vec![],
        post_build_cmds: vec![],
        to_bin: true,
    };

    let ctx = AppContextSpec {
        workspace: manifest_dir.to_path_buf(),
        manifest: manifest_dir.to_path_buf(),
        debug: matches!(config.mode, BuildMode::Debug),
    };

    Ok(CargoBuildSpec { cargo, ctx })
}

pub fn build_qemu_config(config: &ArceosConfig, manifest_dir: &Path) -> QemuConfig {
    let mut args = vec![
        "-machine".to_string(),
        config.arch.to_qemu_machine().to_string(),
        "-cpu".to_string(),
        qemu_cpu(config.arch).to_string(),
        "-m".to_string(),
        config.mem.clone().unwrap_or_else(|| "128M".to_string()),
        "-smp".to_string(),
        config.smp.unwrap_or(1).to_string(),
    ];

    if config.qemu.blk {
        args.push("-device".to_string());
        args.push("virtio-blk-pci,drive=disk0".to_string());
        if let Some(disk_img) = &config.qemu.disk_image {
            args.push("-drive".to_string());
            args.push(format!(
                "id=disk0,if=none,format=raw,file={}",
                disk_img.display()
            ));
        } else {
            let default_disk = manifest_dir.join("disk.img");
            if default_disk.exists() {
                args.push("-drive".to_string());
                args.push(format!(
                    "id=disk0,if=none,format=raw,file={}",
                    default_disk.display()
                ));
            }
        }
    }

    if config.qemu.net {
        args.push("-device".to_string());
        args.push("virtio-net-pci,netdev=net0".to_string());
        args.push("-netdev".to_string());
        args.push(match config.qemu.net_dev {
            NetDev::User => "user,id=net0,hostfwd=tcp::5555-:5555".to_string(),
            NetDev::Tap => "tap,id=net0,script=no".to_string(),
            NetDev::Bridge => "bridge,id=net0,br=virbr0".to_string(),
        });
    }

    if config.qemu.graphic {
        args.push("-device".to_string());
        args.push("virtio-gpu-pci".to_string());
        args.push("-display".to_string());
        args.push("gtk".to_string());
    } else {
        args.push("-nographic".to_string());
        args.push("-serial".to_string());
        args.push("mon:stdio".to_string());
    }

    if config.qemu.accel {
        match config.arch {
            Arch::X86_64 => {
                args.push("-accel".to_string());
                args.push("kvm".to_string());
            }
            Arch::AArch64 => {
                args.push("-accel".to_string());
                args.push("hvf".to_string());
            }
            Arch::RiscV64 | Arch::LoongArch64 => {}
        }
    }

    args.extend(config.qemu.extra_args.iter().cloned());

    QemuConfig {
        args,
        uefi: false,
        to_bin: !matches!(config.arch, Arch::X86_64),
        success_regex: vec!["to install packages.".to_string()],
        fail_regex: vec![],
    }
}

pub fn write_qemu_config(
    manifest_dir: &Path,
    qemu_config_path: &Path,
    config: &ArceosConfig,
) -> Result<PathBuf> {
    let path = qemu_config_path.to_path_buf();
    let qemu = build_qemu_config(config, manifest_dir);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create {}", parent.display()))?;
    }
    fs::write(&path, toml::to_string_pretty(&qemu)?)
        .with_context(|| format!("Failed to write {}", path.display()))?;
    Ok(path)
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
    env.insert("AX_ARCH".to_string(), config.arch.to_string());
    env.insert(
        "AX_PLATFORM".to_string(),
        effective_linker_platform_name(config),
    );
    env.insert("AX_LOG".to_string(), config.log.as_str().to_string());
    env.insert("AX_IP".to_string(), DEFAULT_AX_IP.to_string());
    env.insert("AX_GW".to_string(), DEFAULT_AX_GW.to_string());
    env.insert(
        "AX_CONFIG_PATH".to_string(),
        app_dir.join(AXCONFIG_FILE_NAME).display().to_string(),
    );
    env
}

fn build_cargo_args(config: &ArceosConfig, _manifest_dir: &Path, plat_dyn: bool) -> Vec<String> {
    let mut args = Vec::new();
    args.push("--config".to_string());
    args.push(if plat_dyn {
        format!(
            "target.{}.rustflags=[\"-Clink-arg=-Taxplat.x\"]",
            config.arch.to_target()
        )
    } else {
        format!(
            "target.{}.rustflags=[\"-Clink-arg=-Tlinker.x\",\"-Clink-arg=-no-pie\",\"\
             -Clink-arg=-znostart-stop-gc\"]",
            config.arch.to_target()
        )
    });
    args
}

fn linker_platform_name(platform: &str) -> &str {
    platform.strip_prefix("axplat-").unwrap_or(platform)
}

fn effective_linker_platform_name(config: &ArceosConfig) -> String {
    let platform = config.platform.trim();
    if platform.is_empty() {
        return PlatformResolver::resolve_default_platform_name(&config.arch);
    }

    let normalized = linker_platform_name(platform);
    if arch_matches_platform(config.arch, normalized) {
        normalized.to_string()
    } else {
        PlatformResolver::resolve_default_platform_name(&config.arch)
    }
}

fn arch_matches_platform(arch: Arch, platform: &str) -> bool {
    match arch {
        Arch::X86_64 => platform.starts_with("x86"),
        Arch::AArch64 => platform.starts_with("aarch64"),
        Arch::RiscV64 => platform.starts_with("riscv64"),
        Arch::LoongArch64 => platform.starts_with("loongarch64"),
    }
}

fn qemu_cpu(arch: Arch) -> &'static str {
    match arch {
        Arch::X86_64 => "max",
        Arch::AArch64 => "cortex-a72",
        Arch::RiscV64 => "rv64",
        Arch::LoongArch64 => "la464",
    }
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
