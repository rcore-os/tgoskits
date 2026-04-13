use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, bail};
use indicatif::ProgressBar;
use tokio::fs as tokio_fs;
use xz2::read::XzDecoder;

use crate::{
    context::{ResolvedStarryRequest, starry_target_for_arch_checked},
    download::download_to_path_with_progress,
};

const ROOTFS_URL: &str = "https://github.com/Starry-OS/rootfs/releases/download/20260214";

/// Remove the timeout field from the configuration file
fn remove_timeout_field(config: &str) -> String {
    // Check if config contains timeout line
    if !config.contains("timeout") {
        return config.to_string();
    }
    // Remove timeout line while preserving original format
    config
        .lines()
        .filter(|line| !line.trim().starts_with("timeout"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Update the timeout field in the configuration file to the specified value
fn update_timeout_field(config: &str, timeout_seconds: u64) -> String {
    let timeout_line = format!("timeout = {}", timeout_seconds);
    if config.contains("timeout") {
        // Replace existing timeout line
        config
            .lines()
            .map(|line| {
                if line.trim().starts_with("timeout") {
                    timeout_line.clone()
                } else {
                    line.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        // Add timeout field
        format!("{}\n{}", config, timeout_line)
    }
}

pub(crate) fn rootfs_image_name(arch: &str) -> anyhow::Result<String> {
    let _ = starry_target_for_arch_checked(arch)?;
    Ok(format!("rootfs-{arch}.img"))
}

pub(crate) fn resolve_target_dir(workspace_root: &Path, target: &str) -> anyhow::Result<PathBuf> {
    let _ = crate::context::starry_arch_for_target_checked(target)?;
    Ok(workspace_root.join("target").join(target))
}

fn rootfs_image_path(workspace_root: &Path, arch: &str, target: &str) -> anyhow::Result<PathBuf> {
    let target_dir = resolve_target_dir(workspace_root, target)?;
    Ok(target_dir.join(rootfs_image_name(arch)?))
}

fn shared_rootfs_image_path(target: &str, arch: &str) -> anyhow::Result<String> {
    Ok(format!(
        "${{workspace}}/target/{target}/{}",
        rootfs_image_name(arch)?
    ))
}

pub(crate) async fn ensure_rootfs_in_target_dir(
    workspace_root: &Path,
    arch: &str,
    target: &str,
) -> anyhow::Result<PathBuf> {
    let expected_target = starry_target_for_arch_checked(arch)?;
    if target != expected_target {
        bail!("Starry arch `{arch}` maps to target `{expected_target}`, but got `{target}`");
    }

    let target_dir = resolve_target_dir(workspace_root, target)?;
    tokio_fs::create_dir_all(&target_dir)
        .await
        .with_context(|| format!("failed to create {}", target_dir.display()))?;

    let rootfs_name = rootfs_image_name(arch)?;
    let rootfs_img = rootfs_image_path(workspace_root, arch, target)?;
    let rootfs_xz = target_dir.join(format!("{rootfs_name}.xz"));

    if !rootfs_img.exists() {
        println!("image not found, downloading {}...", rootfs_name);
        let url = format!("{ROOTFS_URL}/{rootfs_name}.xz");
        download_with_progress(&url, &rootfs_xz).await?;
        decompress_xz_file(&rootfs_xz, &rootfs_img).await?;
    }

    Ok(rootfs_img)
}

pub(crate) async fn default_qemu_args(
    workspace_root: &Path,
    request: &ResolvedStarryRequest,
) -> anyhow::Result<Vec<String>> {
    let disk_img =
        ensure_rootfs_in_target_dir(workspace_root, &request.arch, &request.target).await?;
    qemu_args_for_disk_image(disk_img)
}

pub(crate) async fn prepare_test_qemu_config(
    workspace_root: &Path,
    request: &ResolvedStarryRequest,
    template_path: &Path,
    timeout_override: Option<u64>,
) -> anyhow::Result<PathBuf> {
    let base_disk_img =
        ensure_rootfs_in_target_dir(workspace_root, &request.arch, &request.target).await?;
    let isolated_disk_img = isolated_test_disk_image_path(workspace_root, request)?;
    tokio_fs::copy(&base_disk_img, &isolated_disk_img)
        .await
        .with_context(|| {
            format!(
                "failed to copy {} to {}",
                base_disk_img.display(),
                isolated_disk_img.display()
            )
        })?;

    let shared_disk = shared_rootfs_image_path(&request.target, &request.arch)?;
    let config = tokio_fs::read_to_string(template_path)
        .await
        .with_context(|| format!("failed to read {}", template_path.display()))?;
    let config = config.replace(&shared_disk, &isolated_disk_img.display().to_string());

    // Handle timeout override
    let config = match timeout_override {
        None => config,                           // Keep timeout from config file
        Some(0) => remove_timeout_field(&config), // 0 means disable timeout
        Some(seconds) => {
            // Set the specified timeout value
            update_timeout_field(&config, seconds)
        }
    };

    let generated_config = std::env::temp_dir().join(format!(
        "starry-test-qemu-{}-{}-{}.toml",
        request.arch,
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("system time is before unix epoch")?
            .as_nanos()
    ));
    tokio_fs::write(&generated_config, config)
        .await
        .with_context(|| format!("failed to write {}", generated_config.display()))?;

    Ok(generated_config)
}

fn qemu_args_for_disk_image(disk_img: PathBuf) -> anyhow::Result<Vec<String>> {
    Ok(vec![
        "-device".to_string(),
        "virtio-blk-pci,drive=disk0".to_string(),
        "-drive".to_string(),
        format!("id=disk0,if=none,format=raw,file={}", disk_img.display()),
        "-device".to_string(),
        "virtio-net-pci,netdev=net0".to_string(),
        "-netdev".to_string(),
        "user,id=net0".to_string(),
    ])
}

fn isolated_test_disk_image_path(
    workspace_root: &Path,
    request: &ResolvedStarryRequest,
) -> anyhow::Result<PathBuf> {
    let target_dir = resolve_target_dir(workspace_root, &request.target)?;
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system time is before unix epoch")?
        .as_nanos();
    Ok(target_dir.join(format!("disk-test-{}-{timestamp}.img", std::process::id())))
}

async fn download_with_progress(url: &str, output_path: &Path) -> anyhow::Result<()> {
    let client = crate::download::http_client()?;
    download_to_path_with_progress(&client, url, output_path).await
}

async fn decompress_xz_file(input_path: &Path, output_path: &Path) -> anyhow::Result<()> {
    let input_path = input_path.to_path_buf();
    let output_path = output_path.to_path_buf();
    let input_path_for_task = input_path.clone();
    let output_path_for_task = output_path.clone();
    let progress = ProgressBar::new_spinner();
    progress.set_message(format!("decompressing {}", input_path.display()));
    progress.enable_steady_tick(std::time::Duration::from_millis(100));

    tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        let input = fs::File::open(&input_path_for_task)
            .with_context(|| format!("failed to open {}", input_path_for_task.display()))?;
        let output = fs::File::create(&output_path_for_task)
            .with_context(|| format!("failed to create {}", output_path_for_task.display()))?;

        let mut decoder = XzDecoder::new(input);
        let mut writer = std::io::BufWriter::new(output);
        let mut buffer = vec![0u8; 64 * 1024];

        loop {
            let read = decoder.read(&mut buffer).with_context(|| {
                format!("failed to decompress {}", input_path_for_task.display())
            })?;
            if read == 0 {
                break;
            }
            writer
                .write_all(&buffer[..read])
                .with_context(|| format!("failed to write {}", output_path_for_task.display()))?;
        }
        writer
            .flush()
            .with_context(|| format!("failed to flush {}", output_path_for_task.display()))?;
        Ok(())
    })
    .await
    .context("decompression task failed")??;

    progress.finish_with_message(format!("decompressed {}", output_path.display()));
    tokio_fs::remove_file(&input_path)
        .await
        .with_context(|| format!("failed to remove {}", input_path.display()))?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn resolve_target_dir_uses_workspace_target_directory() {
        let root = tempdir().unwrap();
        let dir = resolve_target_dir(root.path(), "x86_64-unknown-none").unwrap();

        assert_eq!(dir, root.path().join("target/x86_64-unknown-none"));
    }

    #[tokio::test]
    async fn default_qemu_args_include_rootfs_and_network_defaults() {
        let root = tempdir().unwrap();
        let target_dir = root.path().join("target/x86_64-unknown-none");
        fs::create_dir_all(&target_dir).unwrap();
        fs::write(target_dir.join("rootfs-x86_64.img"), b"rootfs").unwrap();

        let request = ResolvedStarryRequest {
            package: "starryos".to_string(),
            arch: "x86_64".to_string(),
            target: "x86_64-unknown-none".to_string(),
            plat_dyn: None,
            build_info_path: PathBuf::from("/tmp/.build.toml"),
            qemu_config: None,
            uboot_config: None,
        };

        let args = default_qemu_args(root.path(), &request).await.unwrap();

        assert_eq!(
            args,
            vec![
                "-device".to_string(),
                "virtio-blk-pci,drive=disk0".to_string(),
                "-drive".to_string(),
                format!(
                    "id=disk0,if=none,format=raw,file={}",
                    root.path()
                        .join("target/x86_64-unknown-none/rootfs-x86_64.img")
                        .display()
                ),
                "-device".to_string(),
                "virtio-net-pci,netdev=net0".to_string(),
                "-netdev".to_string(),
                "user,id=net0".to_string(),
            ]
        );
        assert_eq!(
            fs::read(
                root.path()
                    .join("target/x86_64-unknown-none/rootfs-x86_64.img")
            )
            .unwrap(),
            b"rootfs"
        );
    }

    #[tokio::test]
    async fn prepare_test_qemu_config_rewrites_shared_disk_path() {
        let root = tempdir().unwrap();
        let target_dir = root.path().join("target/x86_64-unknown-none");
        fs::create_dir_all(&target_dir).unwrap();
        fs::write(target_dir.join("rootfs-x86_64.img"), b"rootfs").unwrap();
        let template = root.path().join("qemu-x86_64.toml");
        fs::write(
            &template,
            r#"
args = ["-nographic", "-drive", "id=disk0,if=none,format=raw,file=${workspace}/target/x86_64-unknown-none/rootfs-x86_64.img"]
shell_prefix = "starry:~#"
"#,
        )
        .unwrap();

        let request = ResolvedStarryRequest {
            package: "starryos-test".to_string(),
            arch: "x86_64".to_string(),
            target: "x86_64-unknown-none".to_string(),
            plat_dyn: None,
            build_info_path: PathBuf::from("/tmp/.build.toml"),
            qemu_config: None,
            uboot_config: None,
        };

        let generated = prepare_test_qemu_config(root.path(), &request, &template, None)
            .await
            .unwrap();
        let content = fs::read_to_string(generated).unwrap();

        assert!(content.contains("disk-test-"));
        assert!(!content.contains("${workspace}/target/x86_64-unknown-none/rootfs-x86_64.img"));
        assert!(content.contains("shell_prefix = \"starry:~#\""));
    }

    #[test]
    fn remove_timeout_field_removes_timeout_line() {
        let config = r#"args = ["-nographic"]
shell_prefix = "starry:~#"
timeout = 3
"#;
        let result = remove_timeout_field(config);
        assert!(!result.contains("timeout"));
        assert!(result.contains("args = [\"-nographic\"]"));
        assert!(result.contains("shell_prefix = \"starry:~#\""));
    }

    #[test]
    fn remove_timeout_field_handles_config_without_timeout() {
        let config = r#"args = ["-nographic"]
shell_prefix = "starry:~#"
"#;
        let result = remove_timeout_field(config);
        assert_eq!(result, config);
    }

    #[test]
    fn update_timeout_field_replaces_existing_timeout() {
        let config = r#"args = ["-nographic"]
shell_prefix = "starry:~#"
timeout = 3
"#;
        let result = update_timeout_field(config, 10);
        assert!(result.contains("timeout = 10"));
        assert!(!result.contains("timeout = 3"));
    }

    #[test]
    fn update_timeout_field_adds_timeout_when_not_present() {
        let config = r#"args = ["-nographic"]
shell_prefix = "starry:~#"
"#;
        let result = update_timeout_field(config, 30);
        assert!(result.contains("timeout = 30"));
        assert!(result.contains("args = [\"-nographic\"]"));
        assert!(result.contains("shell_prefix = \"starry:~#\""));
    }

    #[test]
    fn update_timeout_field_with_zero_disables_timeout() {
        let config = r#"args = ["-nographic"]
shell_prefix = "starry:~#"
timeout = 3
"#;
        let result = remove_timeout_field(config);
        assert!(!result.contains("timeout"));
    }
}
