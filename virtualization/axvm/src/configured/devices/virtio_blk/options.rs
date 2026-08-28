//! Typed option boundary for AxVM-configured VirtIO block devices.

use std::{format, string::String};

use axvmconfig::VirtualDeviceRequest;

const SECTOR_SIZE: u64 = 512;
const KNOWN_OPTIONS: &[&str] = &[
    "backend",
    "capacity",
    "capacity_sectors",
    "filesystem",
    "path",
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct VirtioBlkOptions {
    pub(super) backend: BackendConfig,
    pub(super) capacity_bytes: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum BackendConfig {
    RamDisk,
    File {
        path: String,
        filesystem: FilesystemFormat,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum FilesystemFormat {
    Ext4,
}

#[derive(Debug, Eq, PartialEq, thiserror::Error)]
pub(super) enum OptionsError {
    #[error("unknown virtio-blk option `{0}`")]
    UnknownField(String),
    #[error("{0}")]
    Invalid(&'static str),
}

impl VirtioBlkOptions {
    pub(super) fn parse(request: &VirtualDeviceRequest) -> Result<Self, OptionsError> {
        reject_unknown_fields(request)?;
        Ok(Self {
            backend: parse_backend(request)?,
            capacity_bytes: parse_capacity(request)?,
        })
    }
}

fn reject_unknown_fields(request: &VirtualDeviceRequest) -> Result<(), OptionsError> {
    if let Some(field) = request
        .options
        .keys()
        .find(|field| !KNOWN_OPTIONS.contains(&field.as_str()))
    {
        return Err(OptionsError::UnknownField(field.clone()));
    }
    Ok(())
}

fn parse_backend(request: &VirtualDeviceRequest) -> Result<BackendConfig, OptionsError> {
    let backend = request
        .options
        .get("backend")
        .map(|value| {
            value.as_str().ok_or(OptionsError::Invalid(
                "`backend` must be `file` or `ramdisk`",
            ))
        })
        .transpose()?
        .unwrap_or("file");

    match backend {
        "ramdisk" => parse_ramdisk_backend(request),
        "file" => parse_file_backend(request),
        _ => Err(OptionsError::Invalid(
            "`backend` must be `file` or `ramdisk`",
        )),
    }
}

fn parse_ramdisk_backend(request: &VirtualDeviceRequest) -> Result<BackendConfig, OptionsError> {
    if request.options.contains_key("path") {
        return Err(OptionsError::Invalid(
            "`path` is only valid for the file backend",
        ));
    }
    if request.options.contains_key("filesystem") {
        return Err(OptionsError::Invalid(
            "`filesystem` is only valid for the file backend",
        ));
    }
    Ok(BackendConfig::RamDisk)
}

fn parse_file_backend(request: &VirtualDeviceRequest) -> Result<BackendConfig, OptionsError> {
    let path = request
        .options
        .get("path")
        .map(|value| {
            value
                .as_str()
                .filter(|path| !path.is_empty())
                .ok_or(OptionsError::Invalid("`path` must be a non-empty string"))
        })
        .transpose()?
        .map(str::to_owned)
        .unwrap_or_else(|| format!("/tmp/{}.img", request.id));
    let filesystem = match request.options.get("filesystem") {
        None => FilesystemFormat::Ext4,
        Some(value) => match value
            .as_str()
            .ok_or(OptionsError::Invalid("`filesystem` must be a string"))?
        {
            "ext4" => FilesystemFormat::Ext4,
            _ => return Err(OptionsError::Invalid("`filesystem` must be `ext4`")),
        },
    };

    Ok(BackendConfig::File { path, filesystem })
}

fn parse_capacity(request: &VirtualDeviceRequest) -> Result<Option<u64>, OptionsError> {
    let capacity = request.options.get("capacity");
    let legacy_sectors = request.options.get("capacity_sectors");
    if capacity.is_some() && legacy_sectors.is_some() {
        return Err(OptionsError::Invalid(
            "specify only one of `capacity` and `capacity_sectors`",
        ));
    }
    if let Some(value) = capacity {
        let value = value
            .as_str()
            .ok_or(OptionsError::Invalid("`capacity` must be a size string"))?;
        return parse_capacity_bytes(value).map(Some);
    }
    if let Some(value) = legacy_sectors {
        let sectors = value
            .as_integer()
            .and_then(|value| u64::try_from(value).ok())
            .filter(|value| *value > 0)
            .ok_or(OptionsError::Invalid("`capacity_sectors` must be positive"))?;
        return sectors
            .checked_mul(SECTOR_SIZE)
            .ok_or(OptionsError::Invalid("`capacity_sectors` is too large"))
            .map(Some);
    }
    Ok(None)
}

fn parse_capacity_bytes(value: &str) -> Result<u64, OptionsError> {
    let split = value
        .find(|character: char| !character.is_ascii_digit())
        .unwrap_or(value.len());
    let (number, suffix) = value.split_at(split);
    let number = number
        .parse::<u64>()
        .ok()
        .filter(|number| *number > 0)
        .ok_or(OptionsError::Invalid(
            "`capacity` must start with a positive integer",
        ))?;
    let multiplier = match suffix.to_ascii_lowercase().as_str() {
        "b" => 1,
        "kb" => 1_000,
        "mb" => 1_000_000,
        "gb" => 1_000_000_000,
        "kib" => 1024,
        "mib" => 1024 * 1024,
        "gib" => 1024 * 1024 * 1024,
        _ => {
            return Err(OptionsError::Invalid(
                "`capacity` suffix must be B, KB, MB, GB, KiB, MiB, or GiB",
            ));
        }
    };
    let bytes = number
        .checked_mul(multiplier)
        .ok_or(OptionsError::Invalid("`capacity` is too large"))?;
    if !bytes.is_multiple_of(SECTOR_SIZE) {
        return Err(OptionsError::Invalid(
            "`capacity` must be a multiple of 512 bytes",
        ));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use axvmconfig::{GuestConfig, VirtualDeviceRequest};

    use super::*;

    #[test]
    fn file_backend_defaults_to_ext4_and_generated_path() {
        let request = request_with_options("");
        assert_eq!(
            VirtioBlkOptions::parse(&request),
            Ok(VirtioBlkOptions {
                backend: BackendConfig::File {
                    path: "/tmp/data.img".into(),
                    filesystem: FilesystemFormat::Ext4,
                },
                capacity_bytes: None,
            })
        );
    }

    #[test]
    fn explicit_ext4_file_backend_is_accepted() {
        let request = request_with_options(
            "backend = \"file\"\npath = \"/tmp/disk.img\"\nfilesystem = \"ext4\"",
        );
        assert!(matches!(
            VirtioBlkOptions::parse(&request),
            Ok(VirtioBlkOptions {
                backend: BackendConfig::File {
                    filesystem: FilesystemFormat::Ext4,
                    ..
                },
                ..
            })
        ));
    }

    #[test]
    fn rejects_unknown_fields_at_the_option_boundary() {
        let error = VirtioBlkOptions::parse(&request_with_options("typo = true"))
            .expect_err("unknown option must be rejected");
        assert_eq!(error.to_string(), "unknown virtio-blk option `typo`");
    }

    #[test]
    fn rejects_invalid_filesystem_values() {
        for filesystem in [r#""fat32""#, r#""""#, r#""EXT4""#, "42"] {
            let request = request_with_options(&format!("filesystem = {filesystem}"));
            assert!(VirtioBlkOptions::parse(&request).is_err());
        }
    }

    #[test]
    fn ramdisk_rejects_file_only_fields() {
        for field in [r#"path = "/tmp/disk.img""#, r#"filesystem = "ext4""#] {
            let request = request_with_options(&format!("backend = \"ramdisk\"\n{field}"));
            assert!(VirtioBlkOptions::parse(&request).is_err());
        }
    }

    #[test]
    fn ramdisk_without_file_fields_remains_compatible() {
        let request = request_with_options(r#"backend = "ramdisk""#);
        assert_eq!(
            VirtioBlkOptions::parse(&request),
            Ok(VirtioBlkOptions {
                backend: BackendConfig::RamDisk,
                capacity_bytes: None,
            })
        );
    }

    #[test]
    fn parses_decimal_binary_and_legacy_capacities() {
        assert_eq!(parse_capacity_bytes("64MB"), Ok(64_000_000));
        assert_eq!(parse_capacity_bytes("2GB"), Ok(2_000_000_000));
        assert_eq!(parse_capacity_bytes("2MiB"), Ok(2 * 1024 * 1024));
        assert_eq!(
            VirtioBlkOptions::parse(&request_with_options("capacity_sectors = 4096"))
                .expect("legacy sectors remain valid")
                .capacity_bytes,
            Some(2 * 1024 * 1024)
        );
    }

    #[test]
    fn rejects_conflicting_invalid_unaligned_and_overflowing_capacities() {
        for options in [
            "capacity = \"1MiB\"\ncapacity_sectors = 2048",
            "capacity = \"0B\"",
            "capacity = \"1KB\"",
            "capacity = \"18446744073709551615GiB\"",
            "capacity_sectors = 0",
            "capacity_sectors = -1",
            "capacity_sectors = 9223372036854775807",
        ] {
            assert!(VirtioBlkOptions::parse(&request_with_options(options)).is_err());
        }
    }

    #[test]
    fn rejects_invalid_backend_and_path_types() {
        for options in [
            "backend = \"memory\"",
            "backend = 42",
            "path = \"\"",
            "path = 42",
        ] {
            assert!(VirtioBlkOptions::parse(&request_with_options(options)).is_err());
        }
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
