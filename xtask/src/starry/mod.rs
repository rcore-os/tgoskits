// Copyright 2026 The tgoskits Team
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
    str::FromStr,
};

use anyhow::{Context, Result, bail};
use axbuild::arceos::{
    ArceosConfigOverride, Arch, AxBuild, BuildMode, FeatureResolver, PlatformResolver,
    QEMU_CONFIG_FILE_NAME, parse_qemu_options, resolve_package_app_dir,
};
use clap::{Parser, Subcommand};
use serde_json::Value;

const ROOTFS_URL: &str = "https://github.com/Starry-OS/rootfs/releases/download/20260214";
const STARRY_PACKAGE: &str = "starryos";

/// StarryOS subcommands
#[derive(Subcommand, Debug)]
pub enum StarryCommand {
    /// Build StarryOS application
    Build {
        #[command(flatten)]
        args: BuildArgs,
    },
    /// Build and run StarryOS application
    Run {
        #[command(flatten)]
        args: RunArgs,
    },

    /// Download rootfs image and place it under target artifact directory
    Rootfs {
        /// Target architecture (default: riscv64)
        #[arg(long)]
        arch: Option<String>,
    },
    /// Deprecated alias for `rootfs`
    Img {
        /// Target architecture (default: riscv64)
        #[arg(long)]
        arch: Option<String>,
    },
}

impl StarryCommand {
    pub async fn run(self) -> Result<()> {
        match self {
            StarryCommand::Build { args } => run_build(args).await,
            StarryCommand::Run { args } => run_with_arg(args).await,
            StarryCommand::Rootfs { arch } => run_rootfs_command(arch),
            StarryCommand::Img { arch } => run_img_command(arch),
        }
    }
}

pub async fn run_test(target: &str) -> Result<()> {
    let arch = parse_starry_target_for_test(target)?;
    let args = RunArgs {
        arch: Some(arch.to_string()),
        package: STARRY_PACKAGE.to_string(),
        platform: None,
        release: true,
        features: None,
        smp: None,
        plat_dyn: false,
        blk: true,
        disk_img: None,
        net: false,
        net_dev: None,
        graphic: false,
        accel: false,
    };

    let run_result = run_with_arg(args).await;
    let cleanup_result = cleanup_generated_qemu_config();

    match (run_result, cleanup_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(run_err), Ok(())) => Err(run_err),
        (Ok(()), Err(cleanup_err)) => Err(cleanup_err),
        (Err(run_err), Err(cleanup_err)) => {
            Err(run_err.context(format!("also failed to cleanup qemu config: {cleanup_err}")))
        }
    }
}

fn cleanup_generated_qemu_config() -> Result<()> {
    let manifest_dir =
        std::env::current_dir().context("failed to get current working directory")?;
    let app_dir = resolve_package_app_dir(&manifest_dir, STARRY_PACKAGE)?;
    let qemu_config_path = manifest_dir.join(app_dir).join(QEMU_CONFIG_FILE_NAME);
    if qemu_config_path.exists() {
        fs::remove_file(&qemu_config_path)
            .with_context(|| format!("failed to remove {}", qemu_config_path.display()))?;
    }
    Ok(())
}

#[derive(Parser, Debug)]
pub struct BuildArgs {
    /// Target architecture (x86_64, aarch64, riscv64, loongarch64)
    #[arg(long)]
    pub arch: Option<String>,

    /// Workspace package name
    #[arg(short = 'p', long = "package", default_value = STARRY_PACKAGE)]
    pub package: String,

    /// Platform name
    #[arg(long)]
    pub platform: Option<String>,

    /// Build in release mode (default: true)
    #[arg(long, default_value_t = true)]
    pub release: bool,

    /// Comma-separated feature list
    #[arg(long)]
    pub features: Option<String>,

    /// Number of CPUs (must be >= 1)
    #[arg(long)]
    pub smp: Option<usize>,

    /// Enable dynamic platform (plat-dyn)
    #[arg(long, action = clap::ArgAction::Set, default_value_t = false)]
    pub plat_dyn: bool,
}

#[derive(Parser, Debug)]
pub struct RunArgs {
    /// Target architecture (x86_64, aarch64, riscv64, loongarch64)
    #[arg(long)]
    pub arch: Option<String>,

    /// Workspace package name
    #[arg(short = 'p', long = "package", default_value = STARRY_PACKAGE)]
    pub package: String,

    /// Platform name
    #[arg(long)]
    pub platform: Option<String>,

    /// Build in release mode (default: true)
    #[arg(long, default_value_t = true)]
    pub release: bool,

    /// Comma-separated feature list
    #[arg(long)]
    pub features: Option<String>,

    /// Number of CPUs (must be >= 1)
    #[arg(long)]
    pub smp: Option<usize>,

    /// Enable dynamic platform (plat-dyn)
    #[arg(long, action = clap::ArgAction::Set, default_value_t = false)]
    pub plat_dyn: bool,

    /// Enable block device
    #[arg(long, default_value_t = true)]
    pub blk: bool,

    /// Disk image path
    #[arg(long)]
    pub disk_img: Option<String>,

    /// Enable network
    #[arg(long, default_value_t = true)]
    pub net: bool,

    /// Network device type (user, tap, bridge)
    #[arg(long)]
    pub net_dev: Option<String>,

    /// Enable graphic output
    #[arg(long)]
    pub graphic: bool,

    /// Enable hardware acceleration (KVM/HVF)
    #[arg(long)]
    pub accel: bool,
}

async fn run_build(args: BuildArgs) -> Result<()> {
    let overrides = build_config_override(
        args.arch,
        args.package.clone(),
        args.platform,
        args.release,
        args.features,
        args.smp,
        args.plat_dyn,
    )?;
    let axbuild = AxBuild::from_overrides(overrides, Some(args.package), None)?;

    println!("Building StarryOS application:");
    let output = axbuild.build().await?;
    println!();
    println!("Build successful!");
    println!("  ELF: {}", output.elf.display());
    println!("  Binary: {}", output.bin.display());
    Ok(())
}

async fn run_with_arg(args: RunArgs) -> Result<()> {
    let arch = parse_starry_arch(args.arch.as_deref())?;
    let default_disk_img = starry_default_disk_image(arch)?;
    let disk_img = args
        .disk_img
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or(default_disk_img.clone());

    if args.blk && !disk_img.exists() {
        println!(
            "disk image missing at {}, preparing rootfs...",
            disk_img.display()
        );
        ensure_rootfs_in_target_dir(arch, &disk_img)?;
    }

    let overrides = run_config_override(
        args.arch,
        args.package.clone(),
        args.platform,
        args.release,
        args.features,
        args.smp,
        args.plat_dyn,
        args.blk,
        Some(disk_img.display().to_string()),
        args.net,
        args.net_dev,
        args.graphic,
        args.accel,
    )?;
    let axbuild = AxBuild::from_overrides(overrides, Some(STARRY_PACKAGE.into()), None)?;
    println!("Running in QEMU...");
    axbuild.run_qemu().await
}

fn run_rootfs_command(arch: Option<String>) -> Result<()> {
    let arch = parse_starry_arch(arch.as_deref())?;
    let disk_img = starry_default_disk_image(arch)?;
    println!("Preparing rootfs for {} at {}...", arch, disk_img.display());
    ensure_rootfs_in_target_dir(arch, &disk_img)?;
    println!("rootfs ready at {}", disk_img.display());
    Ok(())
}

fn run_img_command(arch: Option<String>) -> Result<()> {
    eprintln!(
        "\u{1b}[33mWARN: The 'img' command is deprecated. Please use 'rootfs' instead.\u{1b}[0m"
    );
    run_rootfs_command(arch)
}

fn build_config_override(
    arch: Option<String>,
    _package: String,
    platform: Option<String>,
    release: bool,
    features: Option<String>,
    smp: Option<usize>,
    plat_dyn: bool,
) -> Result<ArceosConfigOverride> {
    let arch = parse_starry_arch(arch.as_deref())?;
    if matches!(smp, Some(0)) {
        bail!("invalid SMP value `0`: SMP must be >= 1");
    }

    Ok(ArceosConfigOverride {
        arch: Some(arch),
        platform: platform.or_else(|| Some(PlatformResolver::resolve_default_platform_name(&arch))),
        mode: release.then_some(BuildMode::Release),
        plat_dyn: Some(plat_dyn),
        smp,
        features: features
            .as_deref()
            .map(FeatureResolver::parse_features)
            .map(Some)
            .unwrap_or(None),
        app_features: Some(vec!["qemu".to_string()]),
        ..Default::default()
    })
}

fn run_config_override(
    arch: Option<String>,
    package: String,
    platform: Option<String>,
    release: bool,
    features: Option<String>,
    smp: Option<usize>,
    plat_dyn: bool,
    blk: bool,
    disk_img: Option<String>,
    net: bool,
    net_dev: Option<String>,
    graphic: bool,
    accel: bool,
) -> Result<ArceosConfigOverride> {
    let mut overrides =
        build_config_override(arch, package, platform, release, features, smp, plat_dyn)?;
    overrides.qemu = Some(parse_qemu_options(
        blk, disk_img, net, net_dev, graphic, accel,
    ));
    Ok(overrides)
}

fn parse_starry_arch(arch: Option<&str>) -> Result<Arch> {
    match arch {
        Some(value) => Arch::from_str(value).context("failed to parse arch override"),
        None => Ok(Arch::RiscV64),
    }
}

fn parse_starry_target_for_test(target: &str) -> Result<Arch> {
    match target {
        "x86_64-unknown-none" => Ok(Arch::X86_64),
        "aarch64-unknown-none-softfloat" => Ok(Arch::AArch64),
        "riscv64gc-unknown-none-elf" => Ok(Arch::RiscV64),
        "loongarch64-unknown-none-softfloat" => Ok(Arch::LoongArch64),
        other => Arch::from_str(other).context("failed to parse starry test target as arch"),
    }
}

fn rootfs_image_name(arch: Arch) -> String {
    format!("rootfs-{}.img", arch)
}

fn starry_default_disk_image(arch: Arch) -> Result<PathBuf> {
    Ok(resolve_starry_artifact_dir(arch)?.join("disk.img"))
}

fn resolve_starry_artifact_dir(arch: Arch) -> Result<PathBuf> {
    let target = arch.to_target();

    let output = Command::new("cargo")
        .arg("build")
        .arg("-p")
        .arg(STARRY_PACKAGE)
        .arg("--target")
        .arg(target)
        .arg("--features")
        .arg("qemu")
        .arg("--message-format=json-render-diagnostics")
        .output()
        .with_context(|| format!("failed to run cargo build for target `{target}`"))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    match parse_artifact_dir_from_cargo_json(&stdout, STARRY_PACKAGE, target) {
        Ok(dir) => {
            if !output.status.success() {
                eprintln!(
                    "WARN: cargo build returned non-zero while resolving artifact directory; \
                     using discovered path: {}",
                    dir.display()
                );
            }
            Ok(dir)
        }
        Err(parse_err) => {
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                bail!(
                    "cargo build failed while resolving starry artifact directory (target \
                     `{}`):\n{}\n\nand JSON parse failed: {}",
                    target,
                    stderr.trim(),
                    parse_err
                );
            }
            Err(parse_err).with_context(|| {
                format!(
                    "failed to parse cargo JSON output for package `{}` and target `{}`",
                    STARRY_PACKAGE, target
                )
            })
        }
    }
}

fn parse_artifact_dir_from_cargo_json(
    stdout: &str,
    package: &str,
    target_triple: &str,
) -> Result<PathBuf> {
    let mut package_match = None;
    let mut triple_match = None;
    let triple_marker = format!("/{target_triple}/");

    for line in stdout.lines() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if value.get("reason").and_then(|v| v.as_str()) != Some("compiler-artifact") {
            continue;
        }
        let is_package_match = value
            .get("target")
            .and_then(|v| v.get("name"))
            .and_then(|v| v.as_str())
            == Some(package);

        if let Some(executable) = value.get("executable").and_then(|v| v.as_str()) {
            let parent = Path::new(executable)
                .parent()
                .context("missing artifact parent directory")?;
            let parent = parent.to_path_buf();
            if is_package_match {
                package_match = Some(parent.clone());
            }
            if executable.contains(&triple_marker) {
                triple_match = Some(parent);
            }
            continue;
        }

        if let Some(filename) = value
            .get("filenames")
            .and_then(|v| v.as_array())
            .and_then(|v| v.first())
            .and_then(|v| v.as_str())
        {
            let parent = Path::new(filename)
                .parent()
                .context("missing artifact parent directory")?;
            let parent = parent.to_path_buf();
            if is_package_match {
                package_match = Some(parent.clone());
            }
            if filename.contains(&triple_marker) {
                triple_match = Some(parent);
            }
        }
    }

    package_match
        .or(triple_match)
        .context("no matching compiler-artifact entry found")
}

fn ensure_rootfs_in_target_dir(arch: Arch, disk_img_path: &Path) -> Result<()> {
    let down_dir = disk_img_path
        .parent()
        .context("disk image path must have a parent directory")?;
    let rootfs_name = rootfs_image_name(arch);
    let rootfs_img = down_dir.join(&rootfs_name);
    let rootfs_xz = down_dir.join(format!("{rootfs_name}.xz"));

    if !rootfs_img.exists() {
        println!("image not found, downloading {}...", rootfs_name);
        let url = format!("{ROOTFS_URL}/{rootfs_name}.xz");
        let status = Command::new("curl")
            .arg("-f")
            .arg("-L")
            .arg(&url)
            .arg("-o")
            .arg(&rootfs_xz)
            .status()
            .with_context(|| format!("failed to spawn curl for {url}"))?;
        if !status.success() {
            bail!("failed to download {}", url);
        }

        let status = Command::new("xz")
            .arg("-d")
            .arg("-f")
            .arg(&rootfs_xz)
            .status()
            .with_context(|| format!("failed to spawn xz for {}", rootfs_xz.display()))?;
        if !status.success() {
            bail!("failed to decompress {}", rootfs_xz.display());
        }
    }

    if let Some(parent) = disk_img_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    fs::copy(&rootfs_img, disk_img_path).with_context(|| {
        format!(
            "failed to copy rootfs image from {} to {}",
            rootfs_img.display(),
            disk_img_path.display()
        )
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn parse_artifact_dir_prefers_executable_path() {
        let json = r#"{"reason":"compiler-artifact","target":{"name":"starryos"},"executable":"/tmp/target/riscv64gc-unknown-none-elf/debug/starryos","filenames":["/tmp/target/riscv64gc-unknown-none-elf/debug/starryos"]}"#;
        let dir =
            parse_artifact_dir_from_cargo_json(json, STARRY_PACKAGE, "riscv64gc-unknown-none-elf")
                .unwrap();
        assert_eq!(
            dir,
            PathBuf::from("/tmp/target/riscv64gc-unknown-none-elf/debug")
        );
    }

    #[test]
    fn parse_artifact_dir_falls_back_to_filenames() {
        let json = r#"{"reason":"compiler-artifact","target":{"name":"starryos"},"filenames":["/tmp/target/riscv64gc-unknown-none-elf/debug/starryos"]}"#;
        let dir =
            parse_artifact_dir_from_cargo_json(json, STARRY_PACKAGE, "riscv64gc-unknown-none-elf")
                .unwrap();
        assert_eq!(
            dir,
            PathBuf::from("/tmp/target/riscv64gc-unknown-none-elf/debug")
        );
    }

    #[test]
    fn parse_artifact_dir_rejects_missing_package() {
        let json = r#"{"reason":"compiler-artifact","target":{"name":"other"},"executable":"/tmp/target/debug/other"}"#;
        let err =
            parse_artifact_dir_from_cargo_json(json, STARRY_PACKAGE, "riscv64gc-unknown-none-elf")
                .unwrap_err();
        assert!(
            err.to_string()
                .contains("no matching compiler-artifact entry")
        );
    }

    #[test]
    fn parse_artifact_dir_falls_back_to_target_triple() {
        let json = r#"{"reason":"compiler-artifact","target":{"name":"other"},"filenames":["/tmp/target/riscv64gc-unknown-none-elf/debug/deps/libother.rlib"]}"#;
        let dir =
            parse_artifact_dir_from_cargo_json(json, STARRY_PACKAGE, "riscv64gc-unknown-none-elf")
                .unwrap();
        assert_eq!(
            dir,
            PathBuf::from("/tmp/target/riscv64gc-unknown-none-elf/debug/deps")
        );
    }

    #[test]
    fn rootfs_image_name_matches_arch() {
        assert_eq!(rootfs_image_name(Arch::RiscV64), "rootfs-riscv64.img");
        assert_eq!(
            rootfs_image_name(Arch::LoongArch64),
            "rootfs-loongarch64.img"
        );
    }

    #[test]
    fn build_args_default_to_release() {
        let args = BuildArgs::parse_from(["build-args"]);
        assert!(args.release);
    }

    #[test]
    fn run_args_default_to_release() {
        let args = RunArgs::parse_from(["run-args"]);
        assert!(args.release);
    }
}
