//! Per-boot guest assets, independent of the Axvisor kernel build.

use std::{
    collections::BTreeSet,
    fs,
    io::{Read, Seek, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, ensure};
use axvmconfig::{BOOT_IMAGE_PATH_FIELDS, BUILTIN_GUEST_DIR, GuestConfig};
use serde::Deserialize;
use sha2::{Digest, Sha256};

#[derive(Default, Deserialize)]
pub(super) struct ResourceInputs {
    pub(super) vm_configs: Option<Vec<PathBuf>>,
    pub(super) busybox_initramfs: Option<String>,
    pub(super) ovmf_firmware: Option<String>,
}
/// ostool-server rejects session-file uploads above 64 MiB, and board FIT
/// images also carry the host kernel. Once the payload approaches that
/// budget, emit the appended guest bundle as one top-level gzip member: the
/// Axvisor host initramfs unpacker inflates it, while nested gzip is not
/// allowed, so the member itself is never already compressed.
const SESSION_UPLOAD_COMPRESS_THRESHOLD: u64 = 48 * 1024 * 1024;

/// Appends the packed guest bundle, gzip-compressed when the resulting
/// payload would otherwise approach the board session upload limit.
fn append_guest_bundle(output: &mut fs::File, bundle: &Path) -> std::io::Result<()> {
    let bundle_size = fs::metadata(bundle)?.len();
    let payload_size = output.stream_position()?.saturating_add(bundle_size);
    if payload_size <= SESSION_UPLOAD_COMPRESS_THRESHOLD {
        std::io::copy(&mut fs::File::open(bundle)?, output)?;
        return Ok(());
    }
    let mut encoder = flate2::write::GzEncoder::new(&mut *output, flate2::Compression::default());
    std::io::copy(&mut fs::File::open(bundle)?, &mut encoder)?;
    encoder.finish()?;
    Ok(())
}
pub(super) fn case_inputs(case_dir: &Path) -> anyhow::Result<ResourceInputs> {
    let manifest = case_dir.join("host-initramfs.toml");
    if !manifest.is_file() {
        return Ok(ResourceInputs::default());
    }
    toml::from_str(&fs::read_to_string(&manifest)?)
        .with_context(|| format!("read {}", manifest.display()))
}

/// Appends a newc bundle to an existing archive, using Linux's archive stream
/// semantics. No boot payload address or kernel compilation state is changed.
pub(super) fn attach(
    vmconfigs: &[PathBuf],
    empty_package: bool,
    output: &Path,
    initramfs: &mut Option<String>,
) -> anyhow::Result<()> {
    attach_with_external_assets(vmconfigs, empty_package, output, initramfs, false)
}

/// Attach board guest configurations while preserving image paths that are
/// supplied by the board root filesystem.  Board HTTP Boot and U-Boot flows
/// still need the configuration in the host archive, but their large guest
/// images may already be installed at paths such as `/linux/...` on the
/// target.  Any image that is available to axbuild is copied into the
/// immutable builtin package and its path is rewritten; unavailable images
/// remain external and are checked on the prepared root before publication.
pub(super) fn attach_with_external_assets(
    vmconfigs: &[PathBuf],
    empty_package: bool,
    output: &Path,
    initramfs: &mut Option<String>,
    allow_external_assets: bool,
) -> anyhow::Result<()> {
    if vmconfigs.is_empty() && !empty_package {
        return Ok(());
    }
    fs::create_dir_all(output.parent().context("bundle output has no parent")?)?;
    let staging = tempfile::tempdir_in(output.parent().context("bundle output has no parent")?)?;
    let builtin = staging
        .path()
        .join(BUILTIN_GUEST_DIR.trim_start_matches('/'));
    fs::create_dir_all(builtin.join("configs"))?;
    fs::create_dir_all(builtin.join("images"))?;
    fs::create_dir_all(builtin.join("symbols"))?;
    let mut ids = BTreeSet::new();
    for config in vmconfigs {
        let raw = fs::read_to_string(config)
            .with_context(|| format!("read guest configuration {}", config.display()))?;
        let guest = GuestConfig::from_toml(&raw)
            .with_context(|| format!("validate guest configuration {}", config.display()))?;
        ensure!(
            ids.insert(guest.base.id),
            "duplicate bundled VM ID {}",
            guest.base.id
        );
        let mut document: toml::Table = toml::from_str(&raw)?;
        let kernel = document
            .get_mut("kernel")
            .and_then(toml::Value::as_table_mut)
            .context("guest configuration requires kernel table")?;
        for field in BOOT_IMAGE_PATH_FIELDS {
            let Some(path) = kernel.get(field).and_then(toml::Value::as_str) else {
                continue;
            };
            let source = Path::new(path);
            let source = if source.is_absolute() {
                source.to_path_buf()
            } else {
                config
                    .parent()
                    .context("guest configuration has no parent")?
                    .join(source)
            };
            if allow_external_assets && !source.is_file() {
                ensure!(
                    Path::new(path).is_absolute(),
                    "missing guest boot asset {} in {}",
                    source.display(),
                    config.display()
                );
                continue;
            }
            ensure!(
                source.is_file(),
                "missing guest boot asset {} in {}",
                source.display(),
                config.display()
            );
            let mut input = fs::File::open(&source)?;
            let mut digest = Sha256::new();
            let mut buffer = [0; 64 * 1024];
            let mut length = 0;
            loop {
                let count = input.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                digest.update(&buffer[..count]);
                length += count;
            }
            ensure!(length != 0, "empty guest boot asset {}", source.display());
            let name = format!("{:x}", digest.finalize());
            let destination = builtin.join("images").join(&name);
            if !destination.exists() {
                fs::copy(&source, &destination)?;
            }
            if let Some(symbols) = [source.with_extension("axbt")]
                .into_iter()
                .find(|candidate| candidate.is_file())
            {
                let symbol_destination = builtin.join("symbols").join(format!("{name}.axbt"));
                if !symbol_destination.exists() {
                    fs::copy(symbols, symbol_destination)?;
                }
            }
            kernel.insert(
                field.into(),
                toml::Value::String(format!("{BUILTIN_GUEST_DIR}/images/{name}")),
            );
        }
        let serialized = toml::to_string_pretty(&document)?;
        GuestConfig::from_toml(&serialized).context("validate bundled guest configuration")?;
        fs::write(
            builtin
                .join("configs")
                .join(format!("vm-{}.toml", guest.base.id)),
            serialized,
        )?;
    }
    let archive = tempfile::NamedTempFile::new_in(output.parent().context("archive parent")?)?;
    crate::image::pack_initramfs_dir(staging.path(), archive.path())?;
    let mut output_file =
        tempfile::NamedTempFile::new_in(output.parent().context("output parent")?)?;
    let mut size = 0;
    if let Some(existing) = initramfs.as_deref() {
        size = std::io::copy(&mut fs::File::open(existing)?, &mut output_file)?;
    }
    while size % 4 != 0 {
        output_file.write_all(&[0])?;
        size += 1;
    }
    append_guest_bundle(output_file.as_file_mut(), archive.path())?;
    output_file.flush()?;
    output_file.persist(output).map_err(|error| error.error)?;
    // Keep the exact packaged configurations available to management clients
    // and host probes that create a VM again after the archive has been freed.
    let configs = output.with_extension("configs");
    if configs.exists() {
        fs::remove_dir_all(&configs)?;
    }
    fs::create_dir_all(&configs)?;
    for entry in fs::read_dir(builtin.join("configs"))? {
        let entry = entry?;
        fs::copy(entry.path(), configs.join(entry.file_name()))?;
    }
    let digest = Sha256::digest(fs::read(output)?);
    println!(
        "Axvisor host initramfs {} sha256={digest:x}",
        output.display()
    );
    *initramfs = Some(output.to_string_lossy().into_owned());
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    #[test]
    fn package_rewrites_all_boot_assets_and_keeps_writable_disks_external() {
        let directory = tempfile::tempdir().unwrap();
        let guest = GuestConfig::default();
        let mut document = toml::Table::try_from(&guest).unwrap();
        let kernel = document.get_mut("kernel").unwrap().as_table_mut().unwrap();
        for field in BOOT_IMAGE_PATH_FIELDS {
            fs::write(directory.path().join(field), field.as_bytes()).unwrap();
            kernel.insert(field.into(), toml::Value::String(field.into()));
        }
        fs::write(directory.path().join("writable.img"), b"private disk").unwrap();
        document
            .get_mut("devices")
            .unwrap()
            .as_table_mut()
            .unwrap()
            .insert(
                "virtual".into(),
                toml::Value::Array(vec![toml::Value::Table(toml::toml! {
                    id = "block"
                    model = "virtio-blk"
                    transport = "pci"
                    path = "/guest/writable.img"
                    filesystem = "ext4"
                })]),
            );
        let config = directory.path().join("guest.toml");
        fs::write(&config, toml::to_string(&document).unwrap()).unwrap();
        let output = directory.path().join("host.cpio");
        let mut archive = None;
        attach(std::slice::from_ref(&config), false, &output, &mut archive).unwrap();
        let entries = parse_newc_entries(&fs::read(&output).unwrap());
        let contents = String::from_utf8(
            entries[&format!("guest/builtin/configs/vm-{}.toml", guest.base.id)].clone(),
        )
        .unwrap();
        let packed = GuestConfig::from_toml(&contents).unwrap();
        assert_eq!(
            packed.kernel.boot_image_paths().count(),
            BOOT_IMAGE_PATH_FIELDS.len()
        );
        for (field, path) in BOOT_IMAGE_PATH_FIELDS
            .into_iter()
            .zip(packed.kernel.boot_image_paths())
        {
            assert!(path.starts_with("/guest/builtin/images/"));
            assert_eq!(entries[path.trim_start_matches('/')], field.as_bytes());
        }
        assert!(contents.contains("/guest/writable.img"));
        assert!(!entries.contains_key("guest/writable.img"));
        let prior = fs::read(&output).unwrap();
        fs::remove_file(directory.path().join("kernel_path")).unwrap();
        assert!(attach(&[config], false, &output, &mut None).is_err());
        assert_eq!(fs::read(&output).unwrap(), prior);
    }

    #[test]
    fn oversized_guest_bundle_is_emitted_as_gzip_member() {
        let directory = tempfile::tempdir().unwrap();
        let mut document = toml::Table::try_from(&GuestConfig::default()).unwrap();
        let asset = directory.path().join("kernel_path");
        fs::write(
            &asset,
            vec![0u8; SESSION_UPLOAD_COMPRESS_THRESHOLD as usize + 1],
        )
        .unwrap();
        document
            .get_mut("kernel")
            .unwrap()
            .as_table_mut()
            .unwrap()
            .insert(
                "kernel_path".into(),
                toml::Value::String("kernel_path".into()),
            );
        let config = directory.path().join("guest.toml");
        fs::write(&config, toml::to_string(&document).unwrap()).unwrap();
        let output = directory.path().join("host.cpio");
        let mut archive = None;

        attach(&[config], false, &output, &mut archive).unwrap();

        let payload = fs::read(&output).unwrap();
        assert_eq!(&payload[..2], &[0x1f, 0x8b]);
        assert!(payload.len() < SESSION_UPLOAD_COMPRESS_THRESHOLD as usize);
        let mut inflated = Vec::new();
        flate2::read::GzDecoder::new(payload.as_slice())
            .read_to_end(&mut inflated)
            .unwrap();
        assert!(inflated.starts_with(b"070701"));
        assert!(inflated.len() > SESSION_UPLOAD_COMPRESS_THRESHOLD as usize);
    }

    #[test]
    fn board_package_keeps_missing_absolute_boot_assets_external() {
        let directory = tempfile::tempdir().unwrap();
        let mut document = toml::Table::try_from(GuestConfig::default()).unwrap();
        document
            .get_mut("kernel")
            .unwrap()
            .as_table_mut()
            .unwrap()
            .insert(
                "kernel_path".into(),
                toml::Value::String("/linux/board-kernel".into()),
            );
        let config = directory.path().join("guest.toml");
        fs::write(&config, toml::to_string(&document).unwrap()).unwrap();
        let output = directory.path().join("host.cpio");
        let mut archive = None;

        attach_with_external_assets(&[config], false, &output, &mut archive, true).unwrap();
        let entries = parse_newc_entries(&fs::read(&output).unwrap());
        let contents = String::from_utf8(
            entries[&format!(
                "guest/builtin/configs/vm-{}.toml",
                GuestConfig::default().base.id
            )]
                .clone(),
        )
        .unwrap();
        let packed = GuestConfig::from_toml(&contents).unwrap();
        assert_eq!(packed.kernel.kernel_path, "/linux/board-kernel");
        assert!(
            entries
                .keys()
                .all(|path| !path.starts_with("guest/builtin/images/"))
        );
    }

    fn parse_newc_entries(archive: &[u8]) -> BTreeMap<String, Vec<u8>> {
        let mut entries = BTreeMap::new();
        let mut offset = 0usize;
        loop {
            let header = &archive[offset..offset + 110];
            assert_eq!(&header[..6], b"070701");
            let field = |index: usize| {
                usize::from_str_radix(
                    std::str::from_utf8(&header[6 + index * 8..14 + index * 8]).unwrap(),
                    16,
                )
                .unwrap()
            };
            let size = field(6);
            let name_size = field(11);
            let name_start = offset + 110;
            let name_end = name_start + name_size;
            let name = std::str::from_utf8(&archive[name_start..name_end - 1])
                .unwrap()
                .to_string();
            let data_start = (name_end + 3) & !3;
            let data_end = data_start + size;
            if name == "TRAILER!!!" {
                break;
            }
            entries.insert(name, archive[data_start..data_end].to_vec());
            offset = (data_end + 3) & !3;
        }
        entries
    }
}
