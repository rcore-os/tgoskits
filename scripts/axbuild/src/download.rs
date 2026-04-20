use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::Context;
use flate2::read::GzDecoder;
use futures_util::StreamExt;
use indicatif::{ProgressBar, ProgressStyle};
use tokio::{fs as tokio_fs, io::AsyncWriteExt};

/// URL of the unified TGOS rootfs tarball.
pub(crate) const UNIFIED_ROOTFS_URL: &str =
    "https://github.com/rcore-os/tgosimages/releases/download/v0.0.1/rootfs.tar.gz";

const UNIFIED_ROOTFS_TARBALL_NAME: &str = "tgosimages-rootfs.tar.gz";

/// Returns the **default** image filename inside the unified rootfs tarball for
/// the given architecture, or `None` when that arch is not included in the
/// tarball.
pub(crate) fn unified_rootfs_image_in_tarball(arch: &str) -> Option<&'static str> {
    match arch {
        "aarch64" => Some("rootfs-aarch64-alpine.img"),
        "riscv64" => Some("rootfs-riscv64-alpine.img"),
        "x86_64" => Some("rootfs-x86_64-alpine.img"),
        "loongarch64" => Some("rootfs-loongarch64-alpine.img"),
        _ => None,
    }
}

/// Directory where all images extracted from the unified tarball are stored:
/// `<workspace_root>/target/rootfs/`.
pub(crate) fn unified_rootfs_dir(workspace_root: &Path) -> PathBuf {
    workspace_root.join("target").join("rootfs")
}

/// Resolves a `--rootfs` CLI argument to an absolute path.
///
/// **Short keywords** (`alpine`, `busybox`, `debian`, …) are expanded to the
/// matching `rootfs-<arch>-<distro>.img` file inside
/// `<workspace_root>/target/rootfs/`:
///
/// | keyword           | image name template              |
/// |-------------------|----------------------------------|
/// | `alpine`/`busybox`| `rootfs-<arch>-busybox.img`      |
/// | `debian`          | `rootfs-<arch>-debian.img`       |
///
/// A **bare filename** (no directory component, not a known keyword) is placed
/// directly inside `target/rootfs/`.
///
/// Any path that contains a directory component (absolute or relative) is
/// returned unchanged.
pub(crate) fn resolve_rootfs_arg(workspace_root: &Path, arch: &str, rootfs: PathBuf) -> PathBuf {
    let is_bare = rootfs
        .parent()
        .map(|p| p.as_os_str().is_empty())
        .unwrap_or(true);

    if !is_bare {
        return rootfs;
    }

    let keyword = rootfs.to_string_lossy();
    let distro = match keyword.as_ref() {
        "alpine" => Some("alpine"),
        "busybox" => Some("busybox"),
        "debian" => Some("debian"),
        _ => None,
    };

    let image_name = if let Some(distro) = distro {
        format!("rootfs-{arch}-{distro}.img")
    } else {
        keyword.into_owned()
    };

    unified_rootfs_dir(workspace_root).join(image_name)
}

/// When `path` is inside `unified_rootfs_dir`, ensures the unified tarball has
/// been downloaded and all its images extracted.  This is a no-op for:
/// - paths outside `unified_rootfs_dir` (user-supplied absolute/relative paths)
/// - architectures not covered by the unified tarball (e.g. loongarch64)
pub(crate) async fn ensure_unified_rootfs_if_managed(
    workspace_root: &Path,
    arch: &str,
    path: &Path,
) -> anyhow::Result<()> {
    if unified_rootfs_image_in_tarball(arch).is_some()
        && path.starts_with(unified_rootfs_dir(workspace_root))
    {
        extract_unified_rootfs_for_arch(workspace_root, arch).await?;
    }
    Ok(())
}

/// Ensures the unified rootfs tarball is downloaded and all its images are
/// extracted to `<workspace_root>/target/rootfs/`.  Returns the path to the
/// architecture-specific image inside that directory.
///
/// Returns an error when `arch` is not covered by the unified tarball.
pub(crate) async fn extract_unified_rootfs_for_arch(
    workspace_root: &Path,
    arch: &str,
) -> anyhow::Result<PathBuf> {
    let image_name = unified_rootfs_image_in_tarball(arch)
        .ok_or_else(|| anyhow!("no unified rootfs image available for arch `{arch}`"))?;

    let rootfs_dir = unified_rootfs_dir(workspace_root);
    let image_path = rootfs_dir.join(image_name);

    if image_path.exists() {
        return Ok(image_path);
    }

    let tarball_dir = workspace_root.join("target");
    tokio_fs::create_dir_all(&tarball_dir)
        .await
        .with_context(|| format!("failed to create {}", tarball_dir.display()))?;

    let tarball_path = tarball_dir.join(UNIFIED_ROOTFS_TARBALL_NAME);
    if !tarball_path.exists() {
        println!("unified rootfs tarball not found, downloading...");
        let client = http_client()?;
        download_to_path_with_progress(&client, UNIFIED_ROOTFS_URL, &tarball_path).await?;
    }

    tokio_fs::create_dir_all(&rootfs_dir)
        .await
        .with_context(|| format!("failed to create {}", rootfs_dir.display()))?;

    let tarball = tarball_path.clone();
    let out_dir = rootfs_dir.clone();
    tokio::task::spawn_blocking(move || extract_all_to_dir(&tarball, &out_dir))
        .await
        .context("rootfs extraction task failed")??;

    Ok(image_path)
}

/// Extracts every file entry from `tarball_path` into `out_dir`, skipping
/// entries that already exist (cache-friendly re-runs).
fn extract_all_to_dir(tarball_path: &Path, out_dir: &Path) -> anyhow::Result<()> {
    let file = fs::File::open(tarball_path)
        .with_context(|| format!("failed to open {}", tarball_path.display()))?;
    let gz = GzDecoder::new(file);
    let mut archive = tar::Archive::new(gz);

    for entry in archive
        .entries()
        .with_context(|| format!("failed to read entries from {}", tarball_path.display()))?
    {
        let mut entry = entry.with_context(|| "failed to read tarball entry")?;
        let raw_path = entry
            .path()
            .with_context(|| "failed to get tarball entry path")?
            .into_owned();
        let Some(name) = raw_path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if name == "." || raw_path.components().count() == 0 {
            continue;
        }
        let name = name.to_owned();
        let dest = out_dir.join(&name);
        if !dest.exists() {
            entry
                .unpack(&dest)
                .with_context(|| format!("failed to extract `{name}` to {}", dest.display()))?;
        }
    }

    Ok(())
}

pub(crate) fn http_client() -> anyhow::Result<reqwest::Client> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(30))
        .timeout(Duration::from_secs(60 * 30))
        .build()
        .map_err(|e| anyhow!("failed to create HTTP client: {e}"))
}

pub(crate) async fn fetch_text(client: &reqwest::Client, url: &str) -> anyhow::Result<String> {
    client
        .get(url)
        .send()
        .await
        .with_context(|| format!("failed to request {url}"))?
        .error_for_status()
        .with_context(|| format!("failed to fetch {url}"))?
        .text()
        .await
        .with_context(|| format!("failed to read response body from {url}"))
}

pub(crate) async fn download_to_path_with_progress(
    client: &reqwest::Client,
    url: &str,
    output_path: &Path,
) -> anyhow::Result<()> {
    let response = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("failed to request {url}"))?
        .error_for_status()
        .with_context(|| format!("failed to download {url}"))?;

    let total_size = response.content_length();
    let progress = new_progress_bar(total_size, output_path);
    let mut file = tokio_fs::File::create(output_path)
        .await
        .with_context(|| format!("failed to create {}", output_path.display()))?;
    let mut stream = response.bytes_stream();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.with_context(|| format!("failed while downloading {url}"))?;
        file.write_all(&chunk)
            .await
            .with_context(|| format!("failed to write {}", output_path.display()))?;
        progress.inc(chunk.len() as u64);
    }

    file.flush()
        .await
        .with_context(|| format!("failed to flush {}", output_path.display()))?;
    progress.finish_with_message(format!("downloaded {}", output_path.display()));
    Ok(())
}

pub(crate) fn new_progress_bar(total_size: Option<u64>, output_path: &Path) -> ProgressBar {
    match total_size {
        Some(total_size) => {
            let progress = ProgressBar::new(total_size);
            progress.set_style(
                ProgressStyle::with_template(
                    "{spinner:.green} {msg} [{bar:40.cyan/blue}] {bytes}/{total_bytes} \
                     ({bytes_per_sec}, ETA {eta})",
                )
                .expect("valid progress bar template")
                .progress_chars("##-"),
            );
            progress.set_message(format!("downloading {}", output_path.display()));
            progress
        }
        None => {
            let progress = ProgressBar::new_spinner();
            progress.set_message(format!("downloading {}", output_path.display()));
            progress.enable_steady_tick(std::time::Duration::from_millis(100));
            progress
        }
    }
}
