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
};

use ::ostool::{
    build::config::Cargo,
    ctx::{AppContext, OutputConfig, PathConfig},
    run::qemu::QemuConfig,
};
use anyhow::{Context, Result};
use serde::Deserialize;

use crate::arceos::config::{ArceosConfig, Arch, BuildMode, NetDev};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppContextSpec {
    pub workspace: PathBuf,
    pub manifest: PathBuf,
    pub build_dir: PathBuf,
    pub bin_dir: Option<PathBuf>,
    pub debug: bool,
}

impl AppContextSpec {
    pub fn into_app_context(self) -> AppContext {
        AppContext {
            paths: PathConfig {
                workspace: self.workspace,
                manifest: self.manifest,
                config: OutputConfig {
                    build_dir: Some(self.build_dir),
                    bin_dir: self.bin_dir,
                },
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

pub fn build_cargo_spec(
    config: &ArceosConfig,
    workspace_root: &Path,
    arceos_dir: &Path,
    target_dir: &Path,
    ax_features: &[String],
    lib_features: &[String],
    use_axlibc: bool,
    plat_dyn: bool,
) -> Result<CargoBuildSpec> {
    let app_dir = app_dir(config, arceos_dir);
    let package = package_name(&app_dir)?;
    let features = build_features(ax_features, lib_features, &config.app_features, use_axlibc);

    let mut args = vec!["--bin".to_string(), package.clone()];
    if plat_dyn {
        args.push("-Z".to_string());
        args.push("build-std=core,alloc".to_string());
    }

    let cargo = Cargo {
        env: build_env(config, arceos_dir, target_dir, use_axlibc, plat_dyn),
        target: config.arch.to_target().to_string(),
        package,
        features,
        log: None,
        extra_config: None,
        args,
        pre_build_cmds: vec![],
        post_build_cmds: vec![],
        to_bin: true,
    };

    let ctx = AppContextSpec {
        workspace: workspace_root.to_path_buf(),
        manifest: app_dir,
        build_dir: target_dir.to_path_buf(),
        bin_dir: config.output_dir.clone(),
        debug: matches!(config.mode, BuildMode::Debug),
    };

    Ok(CargoBuildSpec { cargo, ctx })
}

pub fn build_qemu_config(config: &ArceosConfig, arceos_dir: &Path) -> QemuConfig {
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
            let default_disk = arceos_dir.join("resources/disk.img");
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
        to_bin: false,
        success_regex: vec![],
        fail_regex: vec![],
    }
}

fn app_dir(config: &ArceosConfig, arceos_dir: &Path) -> PathBuf {
    if config.app.is_absolute() {
        config.app.clone()
    } else {
        arceos_dir.join(&config.app)
    }
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

fn build_env(
    config: &ArceosConfig,
    arceos_dir: &Path,
    target_dir: &Path,
    use_axlibc: bool,
    plat_dyn: bool,
) -> HashMap<String, String> {
    let mut env = HashMap::new();
    let rustflags = build_rustflags(config, target_dir, use_axlibc, plat_dyn).join(" ");
    let rustflags = std::env::var("RUSTFLAGS")
        .map(|value| format!("{value} {rustflags}"))
        .unwrap_or(rustflags);

    env.insert("RUSTFLAGS".to_string(), rustflags);
    env.insert("AX_ARCH".to_string(), config.arch.to_string());
    env.insert("AX_PLATFORM".to_string(), config.platform.clone());
    env.insert("AX_LOG".to_string(), config.log.to_string().to_string());
    env.insert(
        "AX_CONFIG_PATH".to_string(),
        arceos_dir.join(".axconfig.toml").display().to_string(),
    );
    env
}

fn build_rustflags(
    config: &ArceosConfig,
    target_dir: &Path,
    use_axlibc: bool,
    plat_dyn: bool,
) -> Vec<String> {
    let target = config.arch.to_target();
    let mode = config.mode.to_string();
    let mut rustflags = vec!["-A".to_string(), "unsafe_op_in_unsafe_fn".to_string()];
    let link_script = target_dir
        .join(target)
        .join(mode)
        .join(format!("linker_{}.lds", config.platform));

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
    let manifest = std::fs::read_to_string(&manifest_path)
        .with_context(|| format!("Failed to read {}", manifest_path.display()))?;
    let manifest: Manifest = toml::from_str(&manifest)
        .with_context(|| format!("Failed to parse {}", manifest_path.display()))?;

    Ok(manifest.package.name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arceos::{QemuOptions, config::LogLevel, features::FeatureResolver};

    fn workspace_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("components directory should exist")
            .parent()
            .expect("workspace root should exist")
            .to_path_buf()
    }

    fn arceos_dir() -> PathBuf {
        workspace_root().join("os/arceos")
    }

    fn helloworld_config() -> ArceosConfig {
        ArceosConfig {
            arch: Arch::X86_64,
            platform: "x86-pc".to_string(),
            app: arceos_dir().join("examples/helloworld"),
            mode: BuildMode::Debug,
            log: LogLevel::Info,
            smp: Some(2),
            mem: Some("256M".to_string()),
            features: vec!["fs".to_string(), "net".to_string()],
            app_features: vec!["custom-app".to_string()],
            qemu: QemuOptions::default(),
            output_dir: None,
        }
    }

    #[test]
    fn test_build_cargo_spec_for_rust_app() {
        let workspace_root = workspace_root();
        let arceos_dir = arceos_dir();
        let target_dir = arceos_dir.join("target");
        let config = helloworld_config();
        let ax_features = FeatureResolver::resolve_ax_features(&config, false);
        let lib_features = FeatureResolver::resolve_lib_features(&config, "axstd");

        let spec = build_cargo_spec(
            &config,
            &workspace_root,
            &arceos_dir,
            &target_dir,
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
        assert_eq!(spec.cargo.args, vec!["--bin", "arceos-helloworld"]);
        assert_eq!(spec.ctx.manifest, config.app);
        assert_eq!(spec.ctx.build_dir, target_dir);
        assert!(spec.ctx.debug);
        assert_eq!(
            spec.cargo.env.get("AX_PLATFORM"),
            Some(&"x86-pc".to_string())
        );
        assert_eq!(spec.cargo.env.get("AX_LOG"), Some(&"info".to_string()));

        let rustflags = spec.cargo.env.get("RUSTFLAGS").unwrap();
        assert!(rustflags.contains("-Clink-arg=-T"));
        assert!(rustflags.contains("linker_x86-pc.lds"));
        assert!(rustflags.contains("-Clink-arg=-no-pie"));
    }

    #[test]
    fn test_build_cargo_spec_maps_debug_and_output_dir() {
        let workspace_root = workspace_root();
        let arceos_dir = arceos_dir();
        let target_dir = arceos_dir.join("target");
        let mut config = helloworld_config();
        config.arch = Arch::AArch64;
        config.platform = "aarch64-qemu-virt".to_string();
        config.mode = BuildMode::Release;
        config.features.clear();
        config.output_dir = Some(PathBuf::from("dist/arceos"));
        let ax_features = FeatureResolver::resolve_ax_features(&config, true);
        let lib_features = FeatureResolver::resolve_lib_features(&config, "axstd");

        let spec = build_cargo_spec(
            &config,
            &workspace_root,
            &arceos_dir,
            &target_dir,
            &ax_features,
            &lib_features,
            false,
            true,
        )
        .unwrap();

        assert!(!spec.ctx.debug);
        assert_eq!(spec.ctx.bin_dir, Some(PathBuf::from("dist/arceos")));
        assert!(
            spec.cargo
                .args
                .windows(2)
                .any(|window| window[0] == "-Z" && window[1] == "build-std=core,alloc")
        );
        let rustflags = spec.cargo.env.get("RUSTFLAGS").unwrap();
        assert!(rustflags.contains("-Crelocation-model=pic"));
        assert!(rustflags.contains("-Clink-arg=-pie"));
        assert!(rustflags.contains("-Clink-arg=-Taxplat.x"));
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

        let qemu = build_qemu_config(&config, &arceos_dir());

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
        assert!(!qemu.to_bin);
    }
}
