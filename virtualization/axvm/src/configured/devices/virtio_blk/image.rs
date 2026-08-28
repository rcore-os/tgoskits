//! ext4 image preparation and publication for configured VirtIO block files.

use core::fmt;
use std::{format, string::String, vec::Vec};

use rsext4::{
    BLOCK_SIZE, BlockIo, DeviceCapabilities, DeviceGeometry, Ext4FileSystem, Ext4Timestamp,
    Jbd2Dev, SectorId,
    error::{Ext4Error, Ext4Result},
};

use super::options::FilesystemFormat;

const SECTOR_SIZE: u64 = 512;

/// Empty file-backed devices use 64 MiB so the default ext4 filesystem can be created and mounted.
pub(super) const DEFAULT_FILE_CAPACITY_BYTES: u64 = 64 * 1024 * 1024;

pub(super) trait ImagePublisher {
    fn len(&self) -> Result<u64, String>;
    fn resize(&mut self, len: u64) -> Result<(), String>;
    fn read_at(&mut self, offset: u64, bytes: &mut [u8]) -> Result<usize, String>;
    fn write_at(&mut self, offset: u64, bytes: &[u8]) -> Result<usize, String>;
    fn flush(&mut self) -> Result<(), String>;
}

pub(super) struct AxFilePublisher {
    file: ax_api::fs::AxFileHandle,
    path: String,
}

impl AxFilePublisher {
    pub(super) fn open(path: &str) -> Result<Self, ImagePreparationError> {
        let mut options = ax_api::fs::AxOpenOptions::new();
        options.read(true);
        options.write(true);
        options.create(true);
        options.direct(true);
        let file = ax_api::fs::ax_open_file(path, &options).map_err(|error| {
            ImagePreparationError::new(
                "open backing file",
                format!("failed to open `{path}`: {error}"),
            )
        })?;
        Ok(Self {
            file,
            path: path.into(),
        })
    }

    pub(super) fn into_file(self) -> ax_api::fs::AxFileHandle {
        self.file
    }
}

impl ImagePublisher for AxFilePublisher {
    fn len(&self) -> Result<u64, String> {
        ax_api::fs::ax_file_attr(&self.file)
            .map(|attributes| attributes.size)
            .map_err(|error| format!("failed to inspect `{}`: {error}", self.path))
    }

    fn resize(&mut self, len: u64) -> Result<(), String> {
        ax_api::fs::ax_truncate_file(&self.file, len)
            .map_err(|error| format!("failed to resize `{}` to {len} bytes: {error}", self.path))
    }

    fn read_at(&mut self, offset: u64, bytes: &mut [u8]) -> Result<usize, String> {
        ax_api::fs::ax_read_file_at(&self.file, offset, bytes)
            .map_err(|error| format!("failed to read `{}` at {offset}: {error}", self.path))
    }

    fn write_at(&mut self, offset: u64, bytes: &[u8]) -> Result<usize, String> {
        ax_api::fs::ax_write_file_at(&self.file, offset, bytes)
            .map_err(|error| format!("failed to write `{}` at {offset}: {error}", self.path))
    }

    fn flush(&mut self) -> Result<(), String> {
        ax_api::fs::ax_flush_file(&self.file)
            .map_err(|error| format!("failed to flush `{}`: {error}", self.path))
    }
}

#[derive(Debug, Eq, PartialEq)]
pub(super) struct ImagePreparationError {
    operation: &'static str,
    detail: String,
    rollback: Option<String>,
}

impl ImagePreparationError {
    pub(super) fn new(operation: &'static str, detail: impl Into<String>) -> Self {
        Self {
            operation,
            detail: detail.into(),
            rollback: None,
        }
    }

    fn with_rollback(mut self, rollback: String) -> Self {
        self.rollback = Some(rollback);
        self
    }
}

impl fmt::Display for ImagePreparationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.operation, self.detail)?;
        if let Some(rollback) = &self.rollback {
            write!(formatter, "; rollback failed: {rollback}")?;
        }
        Ok(())
    }
}

pub(super) fn prepare_file_image<P, A>(
    publisher: &mut P,
    configured_capacity: Option<u64>,
    filesystem: FilesystemFormat,
    allocate: A,
) -> Result<Vec<u8>, ImagePreparationError>
where
    P: ImagePublisher,
    A: FnOnce(u64) -> Result<Vec<u8>, ImagePreparationError>,
{
    let existing_len = publisher
        .len()
        .map_err(|detail| ImagePreparationError::new("inspect backing file", detail))?;
    let capacity = select_capacity(existing_len, configured_capacity)?;
    let mut bytes = allocate(capacity)?;

    if existing_len == 0 {
        format_new_image(&mut bytes, filesystem)?;
        publish_new_image(publisher, &bytes)?;
    } else {
        load_existing_image(publisher, existing_len, capacity, &mut bytes)?;
    }
    Ok(bytes)
}

pub(super) fn allocate_file_mirror(capacity: u64) -> Result<Vec<u8>, ImagePreparationError> {
    let byte_len = usize::try_from(capacity).map_err(|_| {
        ImagePreparationError::new(
            "allocate file mirror",
            "capacity does not fit the host address space",
        )
    })?;
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(byte_len).map_err(|_| {
        ImagePreparationError::new(
            "allocate file mirror",
            "capacity cannot be allocated in the host address space",
        )
    })?;
    bytes.resize(byte_len, 0);
    Ok(bytes)
}

fn select_capacity(
    existing_len: u64,
    configured_capacity: Option<u64>,
) -> Result<u64, ImagePreparationError> {
    let capacity = configured_capacity.unwrap_or(if existing_len == 0 {
        DEFAULT_FILE_CAPACITY_BYTES
    } else {
        existing_len
    });
    if capacity == 0 || !capacity.is_multiple_of(SECTOR_SIZE) {
        return Err(ImagePreparationError::new(
            "validate backing file",
            "capacity must be a positive multiple of 512 bytes",
        ));
    }
    Ok(capacity)
}

fn format_new_image(
    bytes: &mut [u8],
    filesystem: FilesystemFormat,
) -> Result<(), ImagePreparationError> {
    match filesystem {
        FilesystemFormat::Ext4 => format_and_validate_ext4(bytes)
            .map_err(|error| ImagePreparationError::new("format ext4 image", format!("{error}"))),
    }
}

fn format_and_validate_ext4(bytes: &mut [u8]) -> Ext4Result<()> {
    let image = Ext4Image::new(bytes)?;
    let mut device = Jbd2Dev::initial_jbd2dev(0, image, true);
    rsext4::mkfs(&mut device)?;
    let fs = Ext4FileSystem::mount(&mut device)?;
    rsext4::umount(fs, &mut device)
}

fn load_existing_image<P: ImagePublisher>(
    publisher: &mut P,
    existing_len: u64,
    capacity: u64,
    bytes: &mut [u8],
) -> Result<(), ImagePreparationError> {
    let bytes_to_read = usize::try_from(existing_len.min(capacity)).map_err(|_| {
        ImagePreparationError::new(
            "load backing file",
            "backing file length does not fit the host address space",
        )
    })?;
    let read = publisher
        .read_at(0, &mut bytes[..bytes_to_read])
        .map_err(|detail| ImagePreparationError::new("load backing file", detail))?;
    if read != bytes_to_read {
        return Err(ImagePreparationError::new(
            "load backing file",
            format!("backing file returned a short read: read {read} of {bytes_to_read} bytes"),
        ));
    }
    if existing_len != capacity {
        publisher
            .resize(capacity)
            .map_err(|detail| ImagePreparationError::new("resize backing file", detail))?;
    }
    Ok(())
}

fn publish_new_image<P: ImagePublisher>(
    publisher: &mut P,
    bytes: &[u8],
) -> Result<(), ImagePreparationError> {
    let publish_result = publish_image_bytes(publisher, bytes);
    if let Err(error) = publish_result {
        return Err(match rollback_empty_file(publisher) {
            Some(rollback) => error.with_rollback(rollback),
            None => error,
        });
    }
    Ok(())
}

fn publish_image_bytes<P: ImagePublisher>(
    publisher: &mut P,
    bytes: &[u8],
) -> Result<(), ImagePreparationError> {
    publisher
        .resize(bytes.len() as u64)
        .map_err(|detail| ImagePreparationError::new("resize backing file", detail))?;
    let written = publisher
        .write_at(0, bytes)
        .map_err(|detail| ImagePreparationError::new("write backing file", detail))?;
    if written != bytes.len() {
        return Err(ImagePreparationError::new(
            "write backing file",
            format!(
                "backing file returned a short write: wrote {written} of {} bytes",
                bytes.len()
            ),
        ));
    }
    publisher
        .flush()
        .map_err(|detail| ImagePreparationError::new("flush backing file", detail))
}

fn rollback_empty_file<P: ImagePublisher>(publisher: &mut P) -> Option<String> {
    let resize_error = publisher.resize(0).err();
    let flush_error = publisher.flush().err();
    match (resize_error, flush_error) {
        (None, None) => None,
        (Some(resize), None) => Some(resize),
        (None, Some(flush)) => Some(flush),
        (Some(resize), Some(flush)) => Some(format!("{resize}; {flush}")),
    }
}

struct Ext4Image<'a> {
    bytes: &'a mut [u8],
}

impl<'a> Ext4Image<'a> {
    fn new(bytes: &'a mut [u8]) -> Ext4Result<Self> {
        if !bytes.len().is_multiple_of(BLOCK_SIZE) {
            return Err(Ext4Error::invalid_block_size(bytes.len(), BLOCK_SIZE));
        }
        Ok(Self { bytes })
    }

    fn range(
        &self,
        sector: SectorId,
        count: u32,
        buffer_len: usize,
    ) -> Ext4Result<core::ops::Range<usize>> {
        let expected_len = usize::try_from(count)
            .ok()
            .and_then(|count| count.checked_mul(BLOCK_SIZE))
            .ok_or_else(Ext4Error::invalid_input)?;
        if buffer_len != expected_len {
            return Err(Ext4Error::invalid_block_size(buffer_len, expected_len));
        }
        let start = sector
            .as_usize()?
            .checked_mul(BLOCK_SIZE)
            .ok_or_else(Ext4Error::invalid_input)?;
        let end = start
            .checked_add(buffer_len)
            .ok_or_else(Ext4Error::invalid_input)?;
        if end > self.bytes.len() {
            return Err(Ext4Error::block_out_of_range(
                sector.to_u32()?,
                self.block_count(),
            ));
        }
        Ok(start..end)
    }

    fn block_count(&self) -> u64 {
        (self.bytes.len() / BLOCK_SIZE) as u64
    }
}

impl BlockIo for Ext4Image<'_> {
    fn write(&mut self, buffer: &[u8], sector: SectorId, count: u32) -> Ext4Result<()> {
        let range = self.range(sector, count, buffer.len())?;
        self.bytes[range].copy_from_slice(buffer);
        Ok(())
    }

    fn read(&mut self, buffer: &mut [u8], sector: SectorId, count: u32) -> Ext4Result<()> {
        let range = self.range(sector, count, buffer.len())?;
        buffer.copy_from_slice(&self.bytes[range]);
        Ok(())
    }

    fn geometry(&self) -> DeviceGeometry {
        DeviceGeometry::new(BLOCK_SIZE as u32, self.block_count())
    }

    fn capabilities(&self) -> DeviceCapabilities {
        DeviceCapabilities {
            flush: true,
            ..DeviceCapabilities::default()
        }
    }

    fn flush(&mut self) -> Ext4Result<()> {
        Ok(())
    }
}

impl rsext4::Clock for Ext4Image<'_> {
    fn now(&self) -> Ext4Result<Ext4Timestamp> {
        Ok(Ext4Timestamp::UNIX_EPOCH)
    }
}

#[cfg(test)]
mod tests {
    use axvmconfig::{GuestConfig, VirtualDeviceRequest};

    use super::{super::options::*, *};

    const TEST_CAPACITY: u64 = 8 * 1024;
    const EXT4_MAGIC_OFFSET: usize = 1024 + 56;

    #[derive(Default)]
    struct MemoryPublisher {
        bytes: Vec<u8>,
        resize_calls: Vec<u64>,
        read_calls: usize,
        write_calls: usize,
        flush_calls: usize,
        fail_len: bool,
        fail_resize_to: Option<u64>,
        fail_read: bool,
        short_read: bool,
        fail_write: bool,
        short_write: bool,
        fail_flush_calls: Vec<usize>,
    }

    impl ImagePublisher for MemoryPublisher {
        fn len(&self) -> Result<u64, String> {
            if self.fail_len {
                Err("length failed".into())
            } else {
                Ok(self.bytes.len() as u64)
            }
        }

        fn resize(&mut self, len: u64) -> Result<(), String> {
            self.resize_calls.push(len);
            if self.fail_resize_to == Some(len) {
                return Err(format!("resize to {len} failed"));
            }
            let len = usize::try_from(len).map_err(|_| "test length overflow".to_owned())?;
            self.bytes.resize(len, 0);
            Ok(())
        }

        fn read_at(&mut self, offset: u64, bytes: &mut [u8]) -> Result<usize, String> {
            self.read_calls += 1;
            if self.fail_read {
                return Err("read failed".into());
            }
            let start = usize::try_from(offset).map_err(|_| "test offset overflow".to_owned())?;
            let available = self.bytes.len().saturating_sub(start).min(bytes.len());
            let read = if self.short_read {
                available.saturating_sub(1)
            } else {
                available
            };
            bytes[..read].copy_from_slice(&self.bytes[start..start + read]);
            Ok(read)
        }

        fn write_at(&mut self, offset: u64, bytes: &[u8]) -> Result<usize, String> {
            self.write_calls += 1;
            if self.fail_write {
                return Err("write failed".into());
            }
            let start = usize::try_from(offset).map_err(|_| "test offset overflow".to_owned())?;
            let written = if self.short_write {
                bytes.len().saturating_sub(1)
            } else {
                bytes.len()
            };
            self.bytes[start..start + written].copy_from_slice(&bytes[..written]);
            Ok(written)
        }

        fn flush(&mut self) -> Result<(), String> {
            self.flush_calls += 1;
            if self.fail_flush_calls.contains(&self.flush_calls) {
                Err(format!("flush {} failed", self.flush_calls))
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn default_capacity_is_64_mib() {
        assert_eq!(DEFAULT_FILE_CAPACITY_BYTES, 64 * 1024 * 1024);
    }

    #[test]
    fn default_size_request_formats_and_mounts_ext4() {
        let request = request_with_options("");
        let options = VirtioBlkOptions::parse(&request).expect("parse default request");
        let BackendConfig::File { filesystem, .. } = options.backend else {
            panic!("default backend must be a file");
        };
        let mut publisher = MemoryPublisher::default();

        let mut image = prepare_file_image(
            &mut publisher,
            options.capacity_bytes,
            filesystem,
            allocate_file_mirror,
        )
        .expect("prepare default ext4 image");

        assert_eq!(image.len(), DEFAULT_FILE_CAPACITY_BYTES as usize);
        assert_eq!(publisher.bytes, image);
        assert_eq!(ext4_magic(&image), rsext4::EXT4_SUPER_MAGIC);
        assert_ext4_mountable(&mut image);
    }

    #[test]
    fn explicit_ext4_request_formats_requested_capacity() {
        let request = request_with_options("capacity = \"64MiB\"\nfilesystem = \"ext4\"");
        let options = VirtioBlkOptions::parse(&request).expect("parse explicit ext4 request");
        let BackendConfig::File { filesystem, .. } = options.backend else {
            panic!("configured backend must be a file");
        };
        let mut publisher = MemoryPublisher::default();

        let image = prepare_file_image(
            &mut publisher,
            options.capacity_bytes,
            filesystem,
            allocate_file_mirror,
        )
        .expect("prepare explicit ext4 image");

        assert_eq!(image.len(), 64 * 1024 * 1024);
        assert_eq!(ext4_magic(&image), rsext4::EXT4_SUPER_MAGIC);
    }

    #[test]
    fn existing_nonempty_file_preserves_length_and_bytes_without_formatting() {
        let original = vec![0x5a; TEST_CAPACITY as usize];
        let mut publisher = MemoryPublisher {
            bytes: original.clone(),
            ..Default::default()
        };

        let image = prepare_file_image(
            &mut publisher,
            None,
            FilesystemFormat::Ext4,
            allocate_file_mirror,
        )
        .expect("load existing image");

        assert_eq!(image, original);
        assert_eq!(publisher.bytes, original);
        assert!(publisher.resize_calls.is_empty());
        assert_eq!(publisher.write_calls, 0);
        assert_ne!(ext4_magic(&image), rsext4::EXT4_SUPER_MAGIC);
    }

    #[test]
    fn explicit_existing_file_resize_preserves_prefix() {
        let original = vec![0x5a; 4096];
        let mut publisher = MemoryPublisher {
            bytes: original.clone(),
            ..Default::default()
        };

        let image = prepare_file_image(
            &mut publisher,
            Some(TEST_CAPACITY),
            FilesystemFormat::Ext4,
            allocate_file_mirror,
        )
        .expect("resize existing image");

        assert_eq!(&image[..original.len()], original);
        assert_eq!(&publisher.bytes[..original.len()], original);
        assert_eq!(publisher.bytes.len(), TEST_CAPACITY as usize);
        assert_eq!(publisher.resize_calls, [TEST_CAPACITY]);
        assert_eq!(publisher.write_calls, 0);
    }

    #[test]
    fn allocation_and_format_failures_publish_no_filesystem_bytes() {
        let mut allocation_publisher = MemoryPublisher::default();
        let allocation_error = prepare_file_image(
            &mut allocation_publisher,
            Some(TEST_CAPACITY),
            FilesystemFormat::Ext4,
            |_| {
                Err(ImagePreparationError::new(
                    "allocate file mirror",
                    "injected",
                ))
            },
        )
        .expect_err("allocation must fail");
        assert!(allocation_error.to_string().contains("injected"));
        assert!(allocation_publisher.resize_calls.is_empty());
        assert_eq!(allocation_publisher.write_calls, 0);

        let mut format_publisher = MemoryPublisher::default();
        let format_error = prepare_file_image(
            &mut format_publisher,
            Some(1024 * 1024),
            FilesystemFormat::Ext4,
            allocate_file_mirror,
        )
        .expect_err("small ext4 image must fail");
        assert!(format_error.to_string().contains("format ext4 image"));
        assert!(format_publisher.resize_calls.is_empty());
        assert_eq!(format_publisher.write_calls, 0);
    }

    #[test]
    fn image_adapter_rejects_out_of_range_and_invalid_geometry() {
        let mut bytes = [0_u8; BLOCK_SIZE];
        let mut image = Ext4Image::new(&mut bytes).expect("valid image geometry");
        let mut block = [0_u8; BLOCK_SIZE];
        assert!(image.read(&mut block, SectorId::new(1), 1).is_err());
        assert!(Ext4Image::new(&mut [0_u8; 512]).is_err());
    }

    #[test]
    fn invalid_capacity_and_mirror_allocation_return_errors() {
        let mut publisher = MemoryPublisher::default();
        assert!(
            prepare_file_image(
                &mut publisher,
                Some(513),
                FilesystemFormat::Ext4,
                allocate_file_mirror,
            )
            .is_err()
        );
        assert!(allocate_file_mirror(u64::MAX).is_err());
    }

    #[test]
    fn existing_short_read_and_read_failure_do_not_resize() {
        for fail_read in [false, true] {
            let original = vec![0x5a; TEST_CAPACITY as usize];
            let mut publisher = MemoryPublisher {
                bytes: original.clone(),
                fail_read,
                short_read: !fail_read,
                ..Default::default()
            };
            let error = prepare_file_image(
                &mut publisher,
                Some(TEST_CAPACITY * 2),
                FilesystemFormat::Ext4,
                allocate_file_mirror,
            )
            .expect_err("read must fail");
            assert!(error.to_string().contains("load backing file"));
            assert_eq!(publisher.bytes, original);
            assert!(publisher.resize_calls.is_empty());
        }
    }

    #[test]
    fn existing_resize_failure_is_contextual() {
        let mut publisher = MemoryPublisher {
            bytes: vec![0x5a; 4096],
            fail_resize_to: Some(TEST_CAPACITY),
            ..Default::default()
        };
        let error = prepare_file_image(
            &mut publisher,
            Some(TEST_CAPACITY),
            FilesystemFormat::Ext4,
            allocate_file_mirror,
        )
        .expect_err("resize must fail");
        assert_eq!(
            error.to_string(),
            "resize backing file: resize to 8192 failed"
        );
    }

    #[test]
    fn new_image_publication_failures_roll_back_to_zero_length() {
        for failure in ["resize", "write", "short-write", "flush"] {
            let mut publisher = MemoryPublisher::default();
            match failure {
                "resize" => publisher.fail_resize_to = Some(TEST_CAPACITY),
                "write" => publisher.fail_write = true,
                "short-write" => publisher.short_write = true,
                "flush" => publisher.fail_flush_calls.push(1),
                _ => unreachable!(),
            }

            let bytes = vec![0; TEST_CAPACITY as usize];
            let error =
                publish_new_image(&mut publisher, &bytes).expect_err("publication must fail");

            assert!(
                error
                    .to_string()
                    .contains(failure.split('-').next().unwrap()),
                "unexpected error for {failure}: {error}"
            );
            assert_eq!(publisher.bytes.len(), 0, "failure: {failure}");
            assert_eq!(publisher.resize_calls.last(), Some(&0));
        }
    }

    #[test]
    fn rollback_failures_are_attached_to_primary_error_and_flush_is_still_attempted() {
        let mut resize_failure = MemoryPublisher {
            fail_write: true,
            fail_resize_to: Some(0),
            ..Default::default()
        };
        let bytes = vec![0; TEST_CAPACITY as usize];
        let error =
            publish_new_image(&mut resize_failure, &bytes).expect_err("rollback resize must fail");
        assert!(
            error
                .to_string()
                .contains("write backing file: write failed")
        );
        assert!(
            error
                .to_string()
                .contains("rollback failed: resize to 0 failed")
        );
        assert_eq!(resize_failure.flush_calls, 1);

        let mut flush_failure = MemoryPublisher {
            fail_write: true,
            fail_flush_calls: vec![1],
            ..Default::default()
        };
        let error =
            publish_new_image(&mut flush_failure, &bytes).expect_err("rollback flush must fail");
        assert!(
            error
                .to_string()
                .contains("rollback failed: flush 1 failed")
        );
        assert_eq!(flush_failure.bytes.len(), 0);
    }

    #[test]
    fn publisher_length_failure_is_contextual_and_recoverable() {
        let mut publisher = MemoryPublisher {
            fail_len: true,
            ..Default::default()
        };
        let error = prepare_file_image(
            &mut publisher,
            None,
            FilesystemFormat::Ext4,
            allocate_file_mirror,
        )
        .expect_err("length must fail");
        assert_eq!(error.to_string(), "inspect backing file: length failed");
    }

    fn ext4_magic(image: &[u8]) -> u16 {
        u16::from_le_bytes([image[EXT4_MAGIC_OFFSET], image[EXT4_MAGIC_OFFSET + 1]])
    }

    fn assert_ext4_mountable(image: &mut [u8]) {
        let image = Ext4Image::new(image).expect("valid ext4 adapter geometry");
        let mut device = Jbd2Dev::initial_jbd2dev(0, image, true);
        let fs = Ext4FileSystem::mount(&mut device).expect("mount prepared ext4 image");
        rsext4::umount(fs, &mut device).expect("unmount prepared ext4 image");
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
