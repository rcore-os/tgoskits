//! Ext rootfs image resize helpers.
//!
//! This module operates on the rootfs image file itself. It sits beside
//! [`super::inject`] because both modules mutate rootfs images, while CLI
//! modules such as `image` only decide which operation to run.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::Context;

pub(crate) struct ResizeOptions {
    pub(crate) input: PathBuf,
    pub(crate) output: Option<PathBuf>,
    pub(crate) size_mib: u64,
}

pub(crate) fn resize_ext_rootfs_image(options: ResizeOptions) -> anyhow::Result<PathBuf> {
    let image = prepare_resize_target(&options.input, options.output.as_deref())?;
    let target_size = options
        .size_mib
        .checked_mul(1024 * 1024)
        .ok_or_else(|| anyhow::anyhow!("image size is too large: {} MiB", options.size_mib))?;
    let current_size = fs::metadata(&image)
        .with_context(|| format!("failed to stat {}", image.display()))?
        .len();

    if target_size < current_size {
        anyhow::bail!(
            "refusing to shrink {} from {} bytes to {} bytes",
            image.display(),
            current_size,
            target_size
        );
    }

    fs::OpenOptions::new()
        .write(true)
        .open(&image)
        .and_then(|file| file.set_len(target_size))
        .with_context(|| format!("failed to resize {}", image.display()))?;

    run_e2fsck(&image)?;
    run_resize2fs(&image)?;

    Ok(image)
}

fn prepare_resize_target(input: &Path, output: Option<&Path>) -> anyhow::Result<PathBuf> {
    let Some(output) = output else {
        return Ok(input.to_path_buf());
    };

    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    fs::copy(input, output)
        .with_context(|| format!("failed to copy {} to {}", input.display(), output.display()))?;
    Ok(output.to_path_buf())
}

fn run_e2fsck(image: &Path) -> anyhow::Result<()> {
    let e2fsck = find_host_tool(
        "E2FSCK",
        "e2fsck",
        &[
            "/opt/homebrew/opt/e2fsprogs/sbin/e2fsck",
            "/usr/local/opt/e2fsprogs/sbin/e2fsck",
        ],
    )?;
    let status = Command::new(&e2fsck)
        .arg("-fy")
        .arg(image)
        .status()
        .with_context(|| format!("failed to run {}", e2fsck.display()))?;
    match status.code() {
        Some(code) if code & !3 == 0 => {}
        _ => anyhow::bail!("{} -fy failed with {status}", e2fsck.display()),
    }
    Ok(())
}

fn run_resize2fs(image: &Path) -> anyhow::Result<()> {
    let resize2fs = find_host_tool(
        "RESIZE2FS",
        "resize2fs",
        &[
            "/opt/homebrew/opt/e2fsprogs/sbin/resize2fs",
            "/usr/local/opt/e2fsprogs/sbin/resize2fs",
        ],
    )?;
    let status = Command::new(&resize2fs)
        .arg(image)
        .status()
        .with_context(|| format!("failed to run {}", resize2fs.display()))?;
    if !status.success() {
        anyhow::bail!("{} failed with {status}", resize2fs.display());
    }
    Ok(())
}

fn find_host_tool(env_name: &str, tool_name: &str, fallbacks: &[&str]) -> anyhow::Result<PathBuf> {
    if let Some(configured) = std::env::var_os(env_name).filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(configured));
    }
    if let Some(tool) = find_in_path(tool_name) {
        return Ok(tool);
    }
    for fallback in fallbacks {
        let path = PathBuf::from(fallback);
        if path.is_file() {
            return Ok(path);
        }
    }
    anyhow::bail!(
        "{} not found; install it or set {}=/path/to/{}",
        tool_name,
        env_name,
        tool_name
    )
}

fn find_in_path(tool_name: &str) -> Option<PathBuf> {
    let path = Path::new(tool_name);
    if path.components().count() > 1 && path.is_file() {
        return Some(path.to_path_buf());
    }
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join(tool_name))
            .find(|candidate| candidate.is_file())
    })
}
