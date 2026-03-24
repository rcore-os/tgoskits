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
    io::Write,
    path::{Path, PathBuf},
    process::Command,
    str::FromStr,
    time::Instant,
};

use anyhow::{Context, Result, bail};
use axbuild::Arch;
use indicatif::{ProgressBar, ProgressStyle};
use serde_json::Value;
use tracing::{info, warn};
use xz2::read::XzDecoder;

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
    info!(
        "preparing to resolve Starry artifact directory for target `{}`; this runs `cargo build -p {} --target {}` and may take a while",
        target, STARRY_TEST_PACKAGE, target
    );
    let started_at = Instant::now();

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
    info!(
        "finished waiting for artifact directory probe cargo build in {:.2}s",
        started_at.elapsed().as_secs_f64()
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    match parse_artifact_dir_from_cargo_json(stdout, STARRY_TEST_PACKAGE, target) {
        Ok(dir) => {
            if !output.status.success() {
                warn!(
                    "cargo build returned non-zero while resolving artifact directory; using \
                     discovered path: {}",
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

pub async fn ensure_rootfs_in_target_dir(arch: Arch, disk_img: &Path) -> Result<()> {
    let down_dir = disk_img
        .parent()
        .context("disk image path must have a parent directory")?;
    let rootfs_name = rootfs_image_name(arch);
    let rootfs_img = down_dir.join(&rootfs_name);
    let rootfs_xz = down_dir.join(format!("{rootfs_name}.xz"));

    if !rootfs_img.exists() {
        fs::create_dir_all(down_dir)
            .with_context(|| format!("failed to create {}", down_dir.display()))?;
        info!("image not found, downloading {}...", rootfs_name);
        let url = format!("{ROOTFS_URL}/{rootfs_name}.xz");
        download_with_progress(&url, &rootfs_xz)
            .await
            .with_context(|| format!("failed to download {}", url))?;
        decompress_xz_with_progress(&rootfs_xz, &rootfs_img)
            .await
            .with_context(|| format!("failed to decompress {}", rootfs_xz.display()))?;
        fs::remove_file(&rootfs_xz)
            .with_context(|| format!("failed to remove {}", rootfs_xz.display()))?;
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

async fn download_with_progress(url: &str, output_path: &Path) -> Result<()> {
    info!("waiting for network: downloading {url}");
    let client = reqwest::Client::new();
    let mut response = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("request failed for {url}"))?
        .error_for_status()
        .with_context(|| format!("download failed for {url}"))?;

    let total_size = response.content_length();
    let progress = new_download_progress_bar(total_size, output_path);
    let mut output = fs::File::create(output_path)
        .with_context(|| format!("failed to create {}", output_path.display()))?;

    let mut downloaded = 0u64;
    while let Some(chunk) = response
        .chunk()
        .await
        .with_context(|| format!("failed to read response body from {url}"))?
    {
        output
            .write_all(&chunk)
            .with_context(|| format!("failed to write {}", output_path.display()))?;
        downloaded += chunk.len() as u64;
        progress.set_position(downloaded);
    }

    progress.finish_with_message(format!("downloaded {}", output_path.display()));
    Ok(())
}

async fn decompress_xz_with_progress(input_xz: &Path, output_file: &Path) -> Result<()> {
    let input_xz = input_xz.to_path_buf();
    let output_file = output_file.to_path_buf();
    tokio::task::spawn_blocking(move || -> Result<()> {
        let input = fs::File::open(&input_xz)
            .with_context(|| format!("failed to open {}", input_xz.display()))?;
        let input_size = input
            .metadata()
            .with_context(|| format!("failed to read metadata for {}", input_xz.display()))?
            .len();

        let progress = ProgressBar::new(input_size);
        progress.set_style(
            ProgressStyle::with_template(
                "{spinner:.green} {msg} [{wide_bar:.cyan/blue}] {bytes}/{total_bytes} ({eta})",
            )
            .context("invalid decompress progress template")?
            .progress_chars("#>-"),
        );
        progress.set_message(format!("decompressing {}", input_xz.display()));

        let wrapped_input = progress.wrap_read(input);
        let mut decoder = XzDecoder::new(wrapped_input);
        let mut output = fs::File::create(&output_file)
            .with_context(|| format!("failed to create {}", output_file.display()))?;
        std::io::copy(&mut decoder, &mut output).with_context(|| {
            format!(
                "failed to decompress {} into {}",
                input_xz.display(),
                output_file.display()
            )
        })?;
        progress.finish_with_message(format!("decompressed {}", output_file.display()));
        Ok(())
    })
    .await
    .context("decompress task join failed")?
}

fn new_download_progress_bar(total_size: Option<u64>, output_path: &Path) -> ProgressBar {
    let progress = match total_size {
        Some(total) => ProgressBar::new(total),
        None => ProgressBar::new_spinner(),
    };
    progress.set_message(format!("downloading {}", output_path.display()));
    if total_size.is_some() {
        progress.set_style(
            ProgressStyle::with_template(
                "{spinner:.green} {msg} [{wide_bar:.cyan/blue}] {bytes}/{total_bytes} ({eta})",
            )
            .expect("valid progress style template")
            .progress_chars("#>-"),
        );
    } else {
        progress.set_style(
            ProgressStyle::with_template("{spinner:.green} {msg} {bytes}")
                .expect("valid spinner template")
                .tick_chars("|/-\\"),
        );
        progress.enable_steady_tick(std::time::Duration::from_millis(120));
    }
    progress
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
