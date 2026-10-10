//! Guest files that must be present before a VM can be prepared.

use alloc::{format, string::String};

use axvmconfig::{GuestConfig, VirtualDeviceRequest};

/// Returns the first boot or file-backed virtual-device path that is missing.
///
/// The check is deliberately kept next to the VM preparation code. A pool
/// listing and every VM creation entry point must agree on the same set of
/// files, while arbitrary virtual-device options are owned by their model and
/// must not be mistaken for backing paths.
pub(crate) fn missing_guest_image(config: &GuestConfig) -> Option<String> {
    config
        .kernel
        .boot_image_paths()
        .filter(|path| !path.is_empty())
        .find(|path| {
            ax_std::fs::metadata(path)
                .map(|metadata| !metadata.is_file())
                .unwrap_or(true)
        })
        .map(str::to_owned)
        .or_else(|| {
            config
                .devices
                .virtual_devices
                .iter()
                .filter_map(file_backing_path)
                .find(|path| {
                    ax_std::fs::metadata(path)
                        .map(|metadata| !metadata.is_file())
                        .unwrap_or(true)
                })
        })
}

fn file_backing_path(device: &VirtualDeviceRequest) -> Option<String> {
    if device.model != "virtio-blk" {
        return None;
    }
    let backend = device
        .options
        .get("backend")
        .and_then(toml::Value::as_str)
        .unwrap_or("file");
    if backend != "file" {
        return None;
    }
    Some(
        device
            .options
            .get("path")
            .and_then(toml::Value::as_str)
            .filter(|path| !path.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| format!("/tmp/{}.img", device.id)),
    )
}
