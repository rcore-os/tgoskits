//! UEFI guest disk preparation for nested x86 Axvisor tests.

use std::{fs, path::Path};

use anyhow::{Context, ensure};
use ostool::build::config::Cargo;
use tempfile::tempdir;

use crate::{axvisor::rootfs, context::ResolvedAxvisorRequest};

const IMAGE_ENV: &str = "AXVISOR_TEST_X86_UEFI_DISK_IMAGE";
const IMAGE_SIZE: u64 = 256 * 1024 * 1024;
const GUEST_IMAGE_DIRECTORY: &str = "uefi";
const GUEST_IMAGE_NAME: &str = "uefi-guest.img";

pub(super) fn prepare_configured_uefi_disk_image(
    request: &ResolvedAxvisorRequest,
    cargo: &Cargo,
    workspace_root: &Path,
) -> anyhow::Result<()> {
    let Some(configured_image) = cargo.env.get(IMAGE_ENV) else {
        return Ok(());
    };
    ensure!(
        request.arch == "x86_64",
        "{IMAGE_ENV} is only valid for x86_64 Axvisor tests"
    );

    let image_path =
        super::assets::resolve_workspace_path(workspace_root, configured_image, IMAGE_ENV)?;
    validate_uefi_disk_image(&image_path)?;

    let staging = tempdir().context("failed to create UEFI disk staging directory")?;
    let guest_directory = staging.path().join(GUEST_IMAGE_DIRECTORY);
    fs::create_dir(&guest_directory).with_context(|| {
        format!(
            "failed to create UEFI disk staging directory {}",
            guest_directory.display()
        )
    })?;
    let staged_image = guest_directory.join(GUEST_IMAGE_NAME);
    let copied = fs::copy(&image_path, &staged_image).with_context(|| {
        format!(
            "failed to stage UEFI disk image {} as {}",
            image_path.display(),
            staged_image.display()
        )
    })?;
    ensure!(
        copied == IMAGE_SIZE,
        "staged UEFI disk image copied {copied} bytes, expected {IMAGE_SIZE}"
    );

    let rootfs_path = rootfs::qemu_rootfs_path(request, workspace_root, None)?;
    crate::rootfs::inject::inject_overlay(&rootfs_path, staging.path()).with_context(|| {
        format!(
            "failed to inject UEFI disk image into {}",
            rootfs_path.display()
        )
    })
}

fn validate_uefi_disk_image(image_path: &Path) -> anyhow::Result<()> {
    let metadata = fs::symlink_metadata(image_path)
        .with_context(|| format!("failed to inspect UEFI disk image {}", image_path.display()))?;
    ensure!(
        metadata.file_type().is_file(),
        "UEFI disk image is not a regular file: {}",
        image_path.display()
    );
    ensure!(
        metadata.len() == IMAGE_SIZE,
        "UEFI disk image {} is {} bytes, expected {IMAGE_SIZE}",
        image_path.display(),
        metadata.len()
    );
    ensure!(
        metadata.len().is_multiple_of(512),
        "UEFI disk image {} is not sector-aligned",
        image_path.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs::File;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn uefi_disk_image_requires_the_expected_raw_capacity() {
        let root = tempdir().unwrap();
        let image = root.path().join("uefi-guest.img");
        File::create(&image).unwrap().set_len(IMAGE_SIZE).unwrap();

        validate_uefi_disk_image(&image).unwrap();

        File::options()
            .write(true)
            .open(&image)
            .unwrap()
            .set_len(IMAGE_SIZE - 512)
            .unwrap();
        assert!(validate_uefi_disk_image(&image).is_err());
        assert!(validate_uefi_disk_image(root.path()).is_err());
    }
}
