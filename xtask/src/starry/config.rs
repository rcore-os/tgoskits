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
use axbuild::Arch;
use serde_json::Value;

const ROOTFS_URL: &str = "https://github.com/Starry-OS/rootfs/releases/download/20260214";

use super::build::STARRY_TEST_PACKAGE;

pub fn parse_starry_arch(arch: Option<&str>) -> Result<Arch> {
    match arch {
        Some(value) => Arch::from_str(value).context("failed to parse arch override"),
        None => Ok(Arch::RiscV64),
    }
}

pub fn rootfs_image_name(arch: Arch) -> String {
    format!("rootfs-{}.img", arch)
}

pub fn starry_default_disk_image(arch: Arch) -> Result<PathBuf> {
    Ok(resolve_starry_artifact_dir(arch)?.join("disk.img"))
}

pub fn resolve_starry_artifact_dir(arch: Arch) -> Result<PathBuf> {
    let target = arch.to_target();

    let output = Command::new("cargo")
        .arg("build")
        .arg("-p")
        .arg(STARRY_TEST_PACKAGE)
        .arg("--target")
        .arg(target)
        .arg("--features")
        .arg("qemu")
        .arg("--message-format=json-render-diagnostics")
        .output()
        .with_context(|| format!("failed to run cargo build for target `{target}`"))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    match parse_artifact_dir_from_cargo_json(stdout, STARRY_TEST_PACKAGE, target) {
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
                    STARRY_TEST_PACKAGE, target
                )
            })
        }
    }
}

fn parse_artifact_dir_from_cargo_json(
    stdout: impl AsRef<str>,
    package_name: &str,
    target_triple: &str,
) -> Result<PathBuf> {
    let mut executable_match = None;
    let mut package_match = None;
    let mut triple_match = None;
    let triple_marker = format!("/{target_triple}/");

    for line in stdout.as_ref().lines() {
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
            == Some(package_name);

        if let Some(executable) = value.get("executable").and_then(|v| v.as_str()) {
            let parent = normalize_artifact_dir(Path::new(executable))?;
            let parent = parent.to_path_buf();
            if is_package_match {
                package_match = Some(parent.clone());
            }
            if executable.contains(&triple_marker) {
                executable_match = Some(parent.clone());
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
            let parent = normalize_artifact_dir(Path::new(filename))?;
            let parent = parent.to_path_buf();
            if is_package_match {
                package_match = Some(parent.clone());
            }
            if filename.contains(&triple_marker) {
                triple_match = Some(parent);
            }
            continue;
        }
    }

    executable_match
        .or(package_match)
        .or(triple_match)
        .context("no matching compiler-artifact entry found")
}

fn normalize_artifact_dir(path: &Path) -> Result<&Path> {
    let parent = path.parent().context("missing artifact parent directory")?;
    if parent.file_name().and_then(|name| name.to_str()) == Some("deps") {
        return parent
            .parent()
            .context("missing target profile directory for artifact");
    }
    Ok(parent)
}

pub fn ensure_rootfs_in_target_dir(arch: Arch, disk_img: &Path) -> Result<()> {
    let down_dir = disk_img
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

    if let Some(parent) = disk_img.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    fs::copy(&rootfs_img, disk_img).with_context(|| {
        format!(
            "failed to copy {} to {}",
            rootfs_img.display(),
            disk_img.display()
        )
    })?;

    Ok(())
}

pub fn parse_starry_target_for_test(target: &str) -> Result<Arch> {
    match target {
        "x86_64-unknown-none" => Ok(Arch::X86_64),
        "aarch64-unknown-none-softfloat" => Ok(Arch::AArch64),
        "riscv64gc-unknown-none-elf" => Ok(Arch::RiscV64),
        "loongarch64-unknown-none-softfloat" => Ok(Arch::LoongArch64),
        other => Arch::from_str(other).context("failed to parse starry test target as arch"),
    }
}
