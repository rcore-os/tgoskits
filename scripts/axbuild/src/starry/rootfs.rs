use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, bail};
use indicatif::ProgressBar;
use ostool::run::qemu::QemuConfig;
use tokio::fs as tokio_fs;
use xz2::read::XzDecoder;

use crate::{
    context::{ResolvedStarryRequest, starry_target_for_arch_checked},
    download::download_to_path_with_progress,
};

const ROOTFS_URL: &str = "https://github.com/Starry-OS/rootfs/releases/download/20260214";

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

fn per_case_rootfs_path(
    workspace_root: &Path,
    arch: &str,
    target: &str,
    case_name: &str,
) -> anyhow::Result<PathBuf> {
    let target_dir = resolve_target_dir(workspace_root, target)?;
    Ok(target_dir.join(format!("rootfs-{arch}-{case_name}.img")))
}

pub(crate) async fn prepare_per_case_rootfs(
    workspace_root: &Path,
    arch: &str,
    target: &str,
    case_name: &str,
) -> anyhow::Result<PathBuf> {
    let base = rootfs_image_path(workspace_root, arch, target)?;
    let case_rootfs = per_case_rootfs_path(workspace_root, arch, target, case_name)?;

    // Clean up old per-case copy from a previous run
    if case_rootfs.exists() {
        tokio_fs::remove_file(&case_rootfs).await.with_context(|| {
            format!(
                "failed to remove old per-case rootfs {}",
                case_rootfs.display()
            )
        })?;
    }

    // Copy base rootfs to per-case path
    let src = base.clone();
    let dst = case_rootfs.clone();
    tokio::task::spawn_blocking(move || {
        std::fs::copy(&src, &dst)
            .with_context(|| format!("failed to copy {} to {}", src.display(), dst.display()))?;
        Ok::<(), anyhow::Error>(())
    })
    .await
    .context("rootfs copy task failed")??;

    Ok(case_rootfs)
}

pub(crate) async fn apply_default_qemu_args(
    workspace_root: &Path,
    request: &ResolvedStarryRequest,
    qemu: &mut QemuConfig,
) -> anyhow::Result<()> {
    let disk_img =
        ensure_rootfs_in_target_dir(workspace_root, &request.arch, &request.target).await?;
    apply_disk_image_qemu_args(qemu, disk_img);
    Ok(())
}

pub(crate) fn apply_disk_image_qemu_args(qemu: &mut QemuConfig, disk_img: PathBuf) {
    let disk_value = format!("id=disk0,if=none,format=raw,file={}", disk_img.display());
    let args = &mut qemu.args;

    let mut has_blk_device = false;
    let mut has_drive = false;
    let mut has_net_device = false;
    let mut has_netdev = false;

    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "-device" if index + 1 < args.len() => {
                let value = &mut args[index + 1];
                if value == "virtio-blk-pci,drive=disk0" {
                    has_blk_device = true;
                } else if value == "virtio-net-pci,netdev=net0" {
                    has_net_device = true;
                }
                index += 2;
            }
            "-drive" if index + 1 < args.len() => {
                let value = &mut args[index + 1];
                if value.starts_with("id=disk0,if=none,format=raw,file=") {
                    *value = disk_value.clone();
                    has_drive = true;
                }
                index += 2;
            }
            "-netdev" if index + 1 < args.len() => {
                let value = &mut args[index + 1];
                if value == "user,id=net0" {
                    has_netdev = true;
                }
                index += 2;
            }
            _ => index += 1,
        }
    }

    if !has_blk_device {
        args.push("-device".to_string());
        args.push("virtio-blk-pci,drive=disk0".to_string());
    }
    if !has_drive {
        args.push("-drive".to_string());
        args.push(disk_value);
    }
    if !has_net_device {
        args.push("-device".to_string());
        args.push("virtio-net-pci,netdev=net0".to_string());
    }
    if !has_netdev {
        args.push("-netdev".to_string());
        args.push("user,id=net0".to_string());
    }
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
    async fn apply_default_qemu_args_includes_rootfs_and_network_defaults() {
        let root = tempdir().unwrap();
        let target_dir = root.path().join("target/x86_64-unknown-none");
        fs::create_dir_all(&target_dir).unwrap();
        fs::write(target_dir.join("rootfs-x86_64.img"), b"rootfs").unwrap();

        let request = ResolvedStarryRequest {
            package: "starryos".to_string(),
            arch: "x86_64".to_string(),
            target: "x86_64-unknown-none".to_string(),
            plat_dyn: None,
            debug: false,
            build_info_path: PathBuf::from("/tmp/.build.toml"),
            build_info_override: None,
            qemu_config: None,
            uboot_config: None,
        };
        let mut qemu = QemuConfig::default();

        apply_default_qemu_args(root.path(), &request, &mut qemu)
            .await
            .unwrap();

        assert_eq!(
            qemu.args,
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
    async fn apply_default_qemu_args_preserves_existing_base_args() {
        let root = tempdir().unwrap();
        let target_dir = root.path().join("target/riscv64gc-unknown-none-elf");
        fs::create_dir_all(&target_dir).unwrap();
        fs::write(target_dir.join("rootfs-riscv64.img"), b"rootfs").unwrap();

        let request = ResolvedStarryRequest {
            package: "starryos".to_string(),
            arch: "riscv64".to_string(),
            target: "riscv64gc-unknown-none-elf".to_string(),
            plat_dyn: None,
            debug: false,
            build_info_path: PathBuf::from("/tmp/.build.toml"),
            build_info_override: None,
            qemu_config: None,
            uboot_config: None,
        };
        let mut qemu = QemuConfig {
            args: vec![
                "-nographic".to_string(),
                "-cpu".to_string(),
                "rv64".to_string(),
                "-machine".to_string(),
                "virt".to_string(),
            ],
            ..Default::default()
        };

        apply_default_qemu_args(root.path(), &request, &mut qemu)
            .await
            .unwrap();

        assert_eq!(
            qemu.args,
            vec![
                "-nographic".to_string(),
                "-cpu".to_string(),
                "rv64".to_string(),
                "-machine".to_string(),
                "virt".to_string(),
                "-device".to_string(),
                "virtio-blk-pci,drive=disk0".to_string(),
                "-drive".to_string(),
                format!(
                    "id=disk0,if=none,format=raw,file={}",
                    root.path()
                        .join("target/riscv64gc-unknown-none-elf/rootfs-riscv64.img")
                        .display()
                ),
                "-device".to_string(),
                "virtio-net-pci,netdev=net0".to_string(),
                "-netdev".to_string(),
                "user,id=net0".to_string(),
            ]
        );
    }
}
