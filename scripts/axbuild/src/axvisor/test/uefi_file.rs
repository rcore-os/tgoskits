//! Inject a verified GPT boot disk for the nested x86 UEFI file-backend test.

use std::{
    fs::{self, File},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, ensure};
use flate2::read::GzDecoder;
use ostool::build::config::Cargo;
use sha2::{Digest, Sha256};
use tempfile::{TempDir, tempdir_in};

use crate::test::case::copy_file_fast;

const IMAGE_ENV: &str = "AXVISOR_TEST_X86_UEFI_FILE_IMAGE";
const IMAGE_SHA256: &str = "5f7a89a3de40fbb417b91efb3ac7364cf736a5798b15d9ca4695584222945663";
const RAW_SHA256: &str = "103327b27ece60aa594d6bbb422e5ad6343c1bbb0376de69e9e82d8a3ac2eb4f";
const IMAGE_SIZE: usize = 256 * 1024 * 1024;
const ESP_GUID: [u8; 16] = [
    0x28, 0x73, 0x2a, 0xc1, 0x1f, 0xf8, 0xd2, 0x11, 0xba, 0x4b, 0x00, 0xa0, 0xc9, 0x3e, 0xc9, 0x3b,
];

pub(super) struct PreparedBootRootfs {
    _directory: TempDir,
    rootfs_path: PathBuf,
}

impl PreparedBootRootfs {
    pub(super) fn rootfs_path(&self) -> &Path {
        &self.rootfs_path
    }
}

pub(super) fn prepare_configured_boot_rootfs(
    cargo: &Cargo,
    workspace_root: &Path,
    target_dir: &Path,
    shared_rootfs: &Path,
) -> anyhow::Result<Option<PreparedBootRootfs>> {
    let Some(configured_image) = cargo.env.get(IMAGE_ENV) else {
        return Ok(None);
    };
    let source = workspace_file(workspace_root, configured_image, IMAGE_ENV)?;
    let directory = tempdir_in(target_dir).context("failed to create UEFI case directory")?;
    let rootfs_path = directory.path().join("rootfs.img");
    copy_file_fast(shared_rootfs, &rootfs_path)
        .with_context(|| format!("failed to copy Axvisor rootfs {}", shared_rootfs.display()))?;

    let boot = prepare_boot_image(&source)?;
    let overlay = directory.path().join("overlay/guest");
    fs::create_dir_all(&overlay)?;
    let boot_path = overlay.join("boot.img");
    let mut boot_file = File::create(&boot_path)?;
    boot_file.write_all(&boot)?;
    boot_file.sync_all()?;
    crate::rootfs::inject::inject_overlay(&rootfs_path, &directory.path().join("overlay"))
        .with_context(|| format!("failed to inject boot image into {}", rootfs_path.display()))?;
    Ok(Some(PreparedBootRootfs {
        _directory: directory,
        rootfs_path,
    }))
}

fn workspace_file(
    workspace_root: &Path,
    configured: &str,
    variable: &str,
) -> anyhow::Result<PathBuf> {
    let relative = Path::new(configured);
    ensure!(
        !relative.is_absolute()
            && relative
                .components()
                .all(|component| matches!(component, Component::CurDir | Component::Normal(_))),
        "{variable} must be a workspace-relative path without parent traversal"
    );
    let path = workspace_root.join(relative);
    let metadata = fs::symlink_metadata(&path)
        .with_context(|| format!("failed to inspect {}", path.display()))?;
    ensure!(
        metadata.file_type().is_file(),
        "{} is not a regular file",
        path.display()
    );
    ensure!(
        path.canonicalize()?.starts_with(workspace_root),
        "{variable} escapes workspace"
    );
    Ok(path)
}

fn read_asset(path: &Path) -> anyhow::Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        metadata.file_type().is_file(),
        "{} is not a regular file",
        path.display()
    );
    fs::read(path).with_context(|| format!("failed to read {}", path.display()))
}

fn prepare_boot_image(source: &Path) -> anyhow::Result<Vec<u8>> {
    let compressed = read_asset(source)?;
    ensure!(
        sha256(&compressed) == IMAGE_SHA256,
        "UEFI boot gzip digest mismatch"
    );
    let mut boot = Vec::with_capacity(IMAGE_SIZE);
    GzDecoder::new(compressed.as_slice())
        .take(IMAGE_SIZE as u64 + 1)
        .read_to_end(&mut boot)
        .context("failed to decompress UEFI boot image")?;
    ensure!(
        boot.len() == IMAGE_SIZE,
        "UEFI boot image has unexpected capacity"
    );
    ensure!(sha256(&boot) == RAW_SHA256, "UEFI boot raw digest mismatch");
    esp_range(&boot)?;
    Ok(boot)
}

fn esp_range(boot: &[u8]) -> anyhow::Result<(usize, usize)> {
    ensure!(
        boot.len().is_multiple_of(512),
        "boot image is not sector-aligned"
    );
    ensure!(
        &boot[510..512] == b"\x55\xaa",
        "boot image has no protective MBR"
    );
    let header = &boot[512..1024];
    ensure!(&header[..8] == b"EFI PART", "boot image has no GPT header");
    let entries_lba = u64::from_le_bytes(header[72..80].try_into()?);
    let entry_size = u32::from_le_bytes(header[84..88].try_into()?) as u64;
    ensure!(entry_size >= 128, "GPT partition entry is too small");
    let entry_start = usize::try_from(
        entries_lba
            .checked_mul(512)
            .context("GPT entry offset overflow")?,
    )?;
    let entry_end = usize::try_from(
        (entry_start as u64)
            .checked_add(entry_size)
            .context("GPT entry size overflow")?,
    )?;
    let entry = boot
        .get(entry_start..entry_end)
        .context("GPT entry exceeds boot image")?;
    ensure!(entry[..16] == ESP_GUID, "first GPT partition is not an ESP");
    let first_lba = u64::from_le_bytes(entry[32..40].try_into()?);
    let last_lba = u64::from_le_bytes(entry[40..48].try_into()?);
    ensure!(first_lba > 0 && last_lba >= first_lba, "invalid ESP extent");
    let start = usize::try_from(first_lba.checked_mul(512).context("ESP start overflow")?)?;
    let end = usize::try_from(
        last_lba
            .checked_add(1)
            .and_then(|lba| lba.checked_mul(512))
            .context("ESP end overflow")?,
    )?;
    ensure!(end <= boot.len(), "ESP exceeds boot image");
    Ok((start, end))
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    #[test]
    fn compressed_boot_disk_contains_case_configuration() {
        let assets = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../test-suit/axvisor/normal/qemu-uefi-file/assets");
        let mut boot = prepare_boot_image(&assets.join("boot.img.gz"))
            .expect("verify the final GPT/ESP boot disk");
        let (start, end) = esp_range(&boot).unwrap();
        let esp =
            fatfs::FileSystem::new(Cursor::new(&mut boot[start..end]), fatfs::FsOptions::new())
                .unwrap();
        let root = esp.root_dir();
        let mut grub = Vec::new();
        root.open_file("boot/grub/grub.cfg")
            .unwrap()
            .read_to_end(&mut grub)
            .unwrap();
        assert_eq!(grub, fs::read(assets.join("grub.cfg")).unwrap());

        let mut compressed_initramfs = Vec::new();
        root.open_file("boot/initramfs")
            .unwrap()
            .read_to_end(&mut compressed_initramfs)
            .unwrap();
        let mut archive = Vec::new();
        GzDecoder::new(compressed_initramfs.as_slice())
            .read_to_end(&mut archive)
            .unwrap();
        assert_eq!(newc_init(&archive), fs::read(assets.join("init")).unwrap());
    }

    fn newc_init(archive: &[u8]) -> &[u8] {
        let mut offset = 0;
        loop {
            assert_eq!(&archive[offset..offset + 6], b"070701");
            let file_size = hex(&archive[offset + 54..offset + 62]);
            let name_size = hex(&archive[offset + 94..offset + 102]);
            offset += 110;
            let name = &archive[offset..offset + name_size - 1];
            offset = (offset + name_size + 3) & !3;
            assert_ne!(name, b"TRAILER!!!", "init is missing from initramfs");
            let data = &archive[offset..offset + file_size];
            if name == b"init" {
                return data;
            }
            offset = (offset + file_size + 3) & !3;
        }
    }

    fn hex(bytes: &[u8]) -> usize {
        usize::from_str_radix(std::str::from_utf8(bytes).unwrap(), 16).unwrap()
    }
}
