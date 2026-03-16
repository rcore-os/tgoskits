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
use serde::{Deserialize, Serialize};

use crate::arceos::config::{
    ArceosConfig, Arch, BuildMode, NetDev, axconfig_path_for_config, ostool_extra_config_path,
    qemu_config_path_for_config,
};

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
    pub extra_config_path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct CargoExtraConfig {
    target: HashMap<String, TargetExtraConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    unstable: Option<UnstableConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    patch: Option<PatchConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct TargetExtraConfig {
    rustflags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct UnstableConfig {
    #[serde(rename = "build-std")]
    build_std: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct PatchConfig {
    #[serde(rename = "crates-io")]
    crates_io: HashMap<String, PatchEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct PatchEntry {
    path: String,
}

pub fn build_cargo_spec(
    config: &ArceosConfig,
    manifest_dir: &Path,
    ax_features: &[String],
    lib_features: &[String],
    use_axlibc: bool,
    plat_dyn: bool,
) -> Result<CargoBuildSpec> {
    let app_dir = config.app_dir(manifest_dir);
    let cargo_manifest_dir = cargo_default_manifest_dir(&app_dir)?;
    let package = package_name(&app_dir)?;
    let features = build_features(ax_features, lib_features, &config.app_features, use_axlibc);
    let extra_config_path = write_extra_config(manifest_dir, config, use_axlibc, plat_dyn)
        .with_context(|| {
            format!(
                "failed to prepare ostool cargo config under {}",
                manifest_dir.display()
            )
        })?;

    let cargo = Cargo {
        env: build_env(config, manifest_dir),
        target: config.arch.to_target().to_string(),
        package,
        features,
        log: None,
        extra_config: Some(extra_config_path.display().to_string()),
        args: vec![],
        pre_build_cmds: vec![],
        post_build_cmds: vec![],
        to_bin: true,
    };

    let ctx = AppContextSpec {
        workspace: isolated_workspace(manifest_dir),
        manifest: cargo_manifest_dir,
        debug: matches!(config.mode, BuildMode::Debug),
    };

    Ok(CargoBuildSpec {
        cargo,
        ctx,
        extra_config_path,
    })
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
            let default_disk = manifest_dir.join("resources/disk.img");
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
        success_regex: vec![],
        fail_regex: vec![],
    }
}

pub fn write_qemu_config(manifest_dir: &Path, config: &ArceosConfig) -> Result<PathBuf> {
    let path = qemu_config_path_for_config(manifest_dir, config);
    let qemu = build_qemu_config(config, manifest_dir);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create {}", parent.display()))?;
    }
    fs::write(&path, toml::to_string_pretty(&qemu)?)
        .with_context(|| format!("Failed to write {}", path.display()))?;
    Ok(path)
}

pub fn isolated_workspace(manifest_dir: &Path) -> PathBuf {
    manifest_dir.join(".axbuild-ostool")
}

fn build_features(
    ax_features: &[String],
    lib_features: &[String],
    app_features: &[String],
    use_axlibc: bool,
) -> Vec<String> {
    let ax_prefix = if use_axlibc { "axfeat/" } else { "axstd/" };
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

fn build_env(config: &ArceosConfig, manifest_dir: &Path) -> HashMap<String, String> {
    let mut env = HashMap::new();
    env.insert("AX_ARCH".to_string(), config.arch.to_string());
    env.insert("AX_PLATFORM".to_string(), config.platform.clone());
    env.insert("AX_LOG".to_string(), config.log.to_string().to_string());
    env.insert(
        "AX_CONFIG_PATH".to_string(),
        axconfig_path_for_config(manifest_dir, config)
            .display()
            .to_string(),
    );
    env
}

fn build_rustflags(
    config: &ArceosConfig,
    manifest_dir: &Path,
    use_axlibc: bool,
    plat_dyn: bool,
) -> Vec<String> {
    let target = config.arch.to_target();
    let mode = config.mode.to_string();
    let target_dir = resolve_target_dir(config, manifest_dir);
    let mut rustflags = vec!["-A".to_string(), "unsafe_op_in_unsafe_fn".to_string()];
    let link_script = resolve_link_script_path(&target_dir, target, &config.platform, mode);

    if use_axlibc {
        let axlibc_linker = target_dir
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
        rustflags.push(format!("-Clink-arg=-T{}", link_script.display()));
        rustflags.push("-Clink-arg=-no-pie".to_string());
        rustflags.push("-Clink-arg=-znostart-stop-gc".to_string());
    }

    rustflags
}

fn resolve_target_dir(config: &ArceosConfig, manifest_dir: &Path) -> PathBuf {
    let app_dir = config.app_dir(manifest_dir);
    cargo_default_manifest_dir(&app_dir)
        .map(|dir| dir.join("target"))
        .unwrap_or_else(|_| manifest_dir.join("target"))
}

fn resolve_link_script_path(
    target_dir: &Path,
    target: &str,
    platform: &str,
    mode: &str,
) -> PathBuf {
    let file_name = format!("linker_{}.lds", platform);
    let mut modes = vec!["release".to_string(), mode.to_string(), "debug".to_string()];
    modes.dedup();

    for m in &modes {
        let candidate = target_dir.join(target).join(m).join(&file_name);
        if candidate.exists() {
            return candidate;
        }
    }

    target_dir.join(target).join(mode).join(file_name)
}

fn write_extra_config(
    manifest_dir: &Path,
    config: &ArceosConfig,
    use_axlibc: bool,
    plat_dyn: bool,
) -> Result<PathBuf> {
    let path = ostool_extra_config_path(manifest_dir);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create {}", parent.display()))?;
    }

    let mut target = HashMap::new();
    target.insert(
        config.arch.to_target().to_string(),
        TargetExtraConfig {
            rustflags: build_rustflags(config, manifest_dir, use_axlibc, plat_dyn),
        },
    );

    let extra = CargoExtraConfig {
        target,
        unstable: plat_dyn.then(|| UnstableConfig {
            build_std: vec!["core".to_string(), "alloc".to_string()],
        }),
        patch: resolve_axerrno_patch(manifest_dir).map(|path| {
            let mut crates_io = HashMap::new();
            crates_io.insert(
                "axerrno".to_string(),
                PatchEntry {
                    path: path.display().to_string(),
                },
            );
            PatchConfig { crates_io }
        }),
    };

    fs::write(&path, toml::to_string_pretty(&extra)?)
        .with_context(|| format!("Failed to write {}", path.display()))?;
    Ok(path)
}

fn qemu_cpu(arch: Arch) -> &'static str {
    match arch {
        Arch::X86_64 => "max",
        Arch::AArch64 => "cortex-a72",
        Arch::RiscV64 => "rv64",
        Arch::LoongArch64 => "la464",
    }
}

fn resolve_axerrno_patch(manifest_dir: &Path) -> Option<PathBuf> {
    let mut cursor = Some(manifest_dir);
    while let Some(dir) = cursor {
        let candidate = dir.join("components/axerrno");
        if candidate.join("Cargo.toml").exists() {
            return Some(candidate);
        }
        cursor = dir.parent();
    }
    None
}

fn cargo_default_manifest_dir(app_dir: &Path) -> Result<PathBuf> {
    let metadata = MetadataCommand::new()
        .current_dir(app_dir)
        .no_deps()
        .exec()
        .with_context(|| {
            format!(
                "failed to resolve cargo default manifest directory from {}",
                app_dir.display()
            )
        })?;
    Ok(metadata.workspace_root.into_std_path_buf())
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
    use std::{fs, path::Path};

    use tempfile::tempdir;

    use super::*;
    use crate::arceos::{
        FeatureResolver, QemuOptions,
        config::{LogLevel, NetDev},
    };

    fn workspace_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("components directory should exist")
            .parent()
            .expect("workspace root should exist")
            .to_path_buf()
    }

    fn manifest_dir() -> PathBuf {
        workspace_root().join("os/arceos")
    }

    fn helloworld_config() -> ArceosConfig {
        ArceosConfig {
            arch: Arch::X86_64,
            platform: "x86-pc".to_string(),
            app: PathBuf::from("examples/helloworld"),
            mode: BuildMode::Debug,
            log: LogLevel::Info,
            smp: Some(2),
            mem: Some("256M".to_string()),
            features: vec!["fs".to_string(), "net".to_string()],
            app_features: vec!["custom-app".to_string()],
            qemu: QemuOptions::default(),
        }
    }

    #[test]
    fn test_build_cargo_spec_for_rust_app() {
        let manifest_dir = manifest_dir();
        let config = helloworld_config();
        let ax_features = FeatureResolver::resolve_ax_features(&config, false);
        let lib_features = FeatureResolver::resolve_lib_features(&config, "axstd");

        let spec = build_cargo_spec(
            &config,
            &manifest_dir,
            &ax_features,
            &lib_features,
            false,
            false,
        )
        .unwrap();

        assert_eq!(spec.cargo.package, "arceos-helloworld");
        assert_eq!(spec.cargo.target, "x86_64-unknown-none");
        assert!(spec.cargo.features.contains(&"axstd/defplat".to_string()));
        assert!(spec.cargo.features.contains(&"axstd/fs".to_string()));
        assert!(spec.cargo.features.contains(&"axstd/net".to_string()));
        assert!(spec.cargo.features.contains(&"custom-app".to_string()));
        assert_eq!(spec.cargo.log, None);
        assert!(spec.cargo.args.is_empty());
        assert_eq!(spec.ctx.manifest, manifest_dir);
        assert!(spec.ctx.workspace.ends_with(".axbuild-ostool"));
        assert!(spec.ctx.debug);
        assert_eq!(
            spec.cargo.env.get("AX_PLATFORM"),
            Some(&"x86-pc".to_string())
        );
        assert_eq!(spec.cargo.env.get("AX_LOG"), Some(&"info".to_string()));
        assert_eq!(
            spec.cargo.env.get("AX_CONFIG_PATH"),
            Some(
                &manifest_dir
                    .join("examples/helloworld/.axconfig.toml")
                    .display()
                    .to_string()
            )
        );
        assert!(spec.cargo.extra_config.is_some());
    }

    #[test]
    fn test_build_cargo_spec_writes_target_rustflags_and_build_std_to_extra_config() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname=\"demo\"\nversion=\"0.1.0\"\nedition=\"2024\"\n[[bin]]\nname=\"demo\"\
             \npath=\"src/main.rs\"\n",
        )
        .unwrap();
        let mut config = helloworld_config();
        config.arch = Arch::AArch64;
        config.platform = "aarch64-qemu-virt".to_string();
        config.mode = BuildMode::Release;
        config.app = PathBuf::from(".");

        let ax_features = FeatureResolver::resolve_ax_features(&config, true);
        let lib_features = FeatureResolver::resolve_lib_features(&config, "axstd");

        let spec = build_cargo_spec(
            &config,
            dir.path(),
            &ax_features,
            &lib_features,
            false,
            true,
        )
        .unwrap();
        let extra = fs::read_to_string(&spec.extra_config_path).unwrap();
        let parsed: toml::Value = toml::from_str(&extra).unwrap();

        assert!(!spec.ctx.debug);
        assert!(extra.contains("[target.aarch64-unknown-none-softfloat]"));
        assert!(extra.contains("axplat.x"));
        assert!(extra.contains("[unstable]"));
        assert_eq!(
            parsed["unstable"]["build-std"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|value| value.as_str())
                .collect::<Vec<_>>(),
            vec!["core", "alloc"]
        );
        assert!(spec.cargo.args.is_empty());
        assert_eq!(spec.ctx.manifest, dir.path());
    }

    #[test]
    fn test_build_qemu_config_keeps_existing_semantics() {
        let mut config = helloworld_config();
        config.arch = Arch::AArch64;
        config.platform = "aarch64-qemu-virt".to_string();
        config.smp = Some(4);
        config.qemu = QemuOptions {
            blk: true,
            disk_image: Some(PathBuf::from("/tmp/disk.img")),
            net: true,
            net_dev: NetDev::Tap,
            graphic: true,
            accel: true,
            extra_args: vec!["-monitor".to_string(), "none".to_string()],
        };

        let qemu = build_qemu_config(&config, &manifest_dir());

        assert!(!qemu.args.iter().any(|arg| arg == "-kernel"));
        assert!(
            qemu.args
                .windows(2)
                .any(|window| window[0] == "-machine" && window[1] == "virt")
        );
        assert!(
            qemu.args
                .windows(2)
                .any(|window| window[0] == "-cpu" && window[1] == "cortex-a72")
        );
        assert!(
            qemu.args
                .windows(2)
                .any(|window| window[0] == "-m" && window[1] == "256M")
        );
        assert!(
            qemu.args
                .windows(2)
                .any(|window| window[0] == "-smp" && window[1] == "4")
        );
        assert!(
            qemu.args
                .iter()
                .any(|arg| arg == "virtio-blk-pci,drive=disk0")
        );
        assert!(qemu.args.iter().any(|arg| arg == "tap,id=net0,script=no"));
        assert!(qemu.args.iter().any(|arg| arg == "virtio-gpu-pci"));
        assert!(
            qemu.args
                .windows(2)
                .any(|window| window[0] == "-accel" && window[1] == "hvf")
        );
        assert!(
            qemu.args
                .ends_with(&["-monitor".to_string(), "none".to_string()])
        );
        assert!(qemu.to_bin);
    }

    #[test]
    fn test_resolve_link_script_path_prefers_existing_release_script() {
        let dir = tempdir().unwrap();
        let target_dir = dir.path().join("target");
        let release_script = target_dir
            .join("riscv64gc-unknown-none-elf")
            .join("release")
            .join("linker_riscv64-qemu-virt.lds");
        fs::create_dir_all(release_script.parent().unwrap()).unwrap();
        fs::write(&release_script, "/* linker */").unwrap();

        let resolved = resolve_link_script_path(
            &target_dir,
            "riscv64gc-unknown-none-elf",
            "riscv64-qemu-virt",
            "debug",
        );
        assert_eq!(resolved, release_script);
    }

    #[test]
    fn test_write_qemu_config_targets_app_dir() {
        let dir = tempdir().unwrap();
        let app_dir = dir.path().join("examples/helloworld");
        fs::create_dir_all(&app_dir).unwrap();
        let path = write_qemu_config(dir.path(), &helloworld_config()).unwrap();
        assert_eq!(path, app_dir.join(".qemu.toml"));
        assert!(path.exists());
    }
}
