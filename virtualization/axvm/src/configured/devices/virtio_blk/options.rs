//! Typed backend options for AxVM-configured VirtIO block devices.

use std::string::String;

use axvmconfig::VirtualDeviceRequest;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FilesystemFormat {
    #[cfg(all(test, feature = "fs"))]
    Unformatted,
    Ext4,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum BackendConfig {
    RamDisk,
    File {
        path: String,
        filesystem: FilesystemFormat,
    },
}

pub(crate) fn parse_backend(request: &VirtualDeviceRequest) -> Result<BackendConfig, &'static str> {
    let backend = request
        .options
        .get("backend")
        .map(|value| {
            value
                .as_str()
                .ok_or("`backend` must be `file` or `ramdisk`")
        })
        .transpose()?
        .unwrap_or("file");

    match backend {
        "ramdisk" => parse_ramdisk_backend(request),
        "file" => parse_file_backend(request),
        _ => Err("`backend` must be `file` or `ramdisk`"),
    }
}

fn parse_ramdisk_backend(request: &VirtualDeviceRequest) -> Result<BackendConfig, &'static str> {
    if request.options.contains_key("path") {
        return Err("`path` is only valid for the file backend");
    }
    if request.options.contains_key("filesystem") {
        return Err("`filesystem` is only valid for the file backend");
    }
    Ok(BackendConfig::RamDisk)
}

fn parse_file_backend(request: &VirtualDeviceRequest) -> Result<BackendConfig, &'static str> {
    let path = request
        .options
        .get("path")
        .map(|value| {
            value
                .as_str()
                .filter(|path| !path.is_empty())
                .ok_or("`path` must be a non-empty string")
        })
        .transpose()?
        .map(str::to_owned)
        .unwrap_or_else(|| std::format!("/tmp/{}.img", request.id));
    let filesystem = match request.options.get("filesystem") {
        None => FilesystemFormat::Ext4,
        Some(value) => match value.as_str().ok_or("`filesystem` must be a string")? {
            "ext4" => FilesystemFormat::Ext4,
            _ => return Err("`filesystem` must be `ext4`"),
        },
    };

    Ok(BackendConfig::File { path, filesystem })
}

#[cfg(test)]
mod tests {
    use std::format;

    use axvmconfig::{GuestConfig, VirtualDeviceRequest};

    use super::*;

    #[test]
    fn omitted_filesystem_selects_ext4_file() {
        let request = request_with_options(r#"path = "/tmp/data.img""#);

        assert_eq!(
            parse_backend(&request),
            Ok(BackendConfig::File {
                path: "/tmp/data.img".into(),
                filesystem: FilesystemFormat::Ext4,
            })
        );
    }

    #[test]
    fn explicit_ext4_selects_ext4_file() {
        let request = request_with_options(
            r#"
path = "/tmp/data.img"
filesystem = "ext4"
"#,
        );

        assert_eq!(
            parse_backend(&request),
            Ok(BackendConfig::File {
                path: "/tmp/data.img".into(),
                filesystem: FilesystemFormat::Ext4,
            })
        );
    }

    #[test]
    fn rejects_unknown_empty_uppercase_and_non_string_filesystems() {
        for filesystem in [r#""fat32""#, r#""""#, r#""EXT4""#, "42"] {
            let request = request_with_options(&format!("filesystem = {filesystem}"));

            assert!(
                parse_backend(&request).is_err(),
                "filesystem value {filesystem} must be rejected"
            );
        }
    }

    #[test]
    fn ramdisk_rejects_any_filesystem_field() {
        for filesystem in [r#""ext4""#, r#""fat32""#, "42"] {
            let request =
                request_with_options(&format!("backend = \"ramdisk\"\nfilesystem = {filesystem}"));

            assert!(
                parse_backend(&request).is_err(),
                "ramdisk filesystem value {filesystem} must be rejected"
            );
        }
    }

    #[test]
    fn ramdisk_without_filesystem_remains_valid() {
        let request = request_with_options(r#"backend = "ramdisk""#);

        assert_eq!(parse_backend(&request), Ok(BackendConfig::RamDisk));
    }

    fn request_with_options(options: &str) -> VirtualDeviceRequest {
        let config = GuestConfig::from_toml(&format!(
            r#"
[devices]
[[devices.virtual]]
id = "data"
model = "virtio-blk"
{options}
"#
        ))
        .expect("parse test guest configuration");
        config
            .devices
            .virtual_devices
            .into_iter()
            .next()
            .expect("test guest has one virtual device")
    }
}
