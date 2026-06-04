use uefi::{boot, proto::loaded_image::LoadedImage};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManifestUrlError {
    LoadedImageUnavailable,
    MissingFilePath,
    InvalidDevicePath,
}

pub fn manifest_url_from_loaded_image(buffer: &mut [u8]) -> Result<&str, ManifestUrlError> {
    let image = boot::open_protocol_exclusive::<LoadedImage>(boot::image_handle())
        .map_err(|_| ManifestUrlError::LoadedImageUnavailable)?;
    let file_path = image.file_path().ok_or(ManifestUrlError::MissingFilePath)?;
    let loader_url = bootloader_common::uri_from_device_path(file_path.as_bytes())
        .map_err(|_| ManifestUrlError::InvalidDevicePath)?;
    bootloader_common::write_sibling_manifest_url(loader_url, buffer)
        .map_err(|_| ManifestUrlError::InvalidDevicePath)
}
