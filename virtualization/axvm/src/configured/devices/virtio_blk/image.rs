//! ext4 initialization and publication for AxVM VirtIO block image mirrors.

use core::fmt;
use std::{format, string::String, vec::Vec};

use rsext4::{
    BLOCK_SIZE, BlockIo, DeviceCapabilities, DeviceGeometry, Ext4FileSystem, Ext4Timestamp,
    Jbd2Dev, SectorId,
    error::{Ext4Error, Ext4Result},
};

use super::options::FilesystemFormat;

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

fn format_ext4_image(bytes: &mut [u8]) -> Ext4Result<()> {
    let image = Ext4Image::new(bytes)?;
    let mut device = Jbd2Dev::initial_jbd2dev(0, image, true);
    rsext4::mkfs(&mut device)?;
    let fs = Ext4FileSystem::mount(&mut device)?;
    rsext4::umount(fs, &mut device)
}

pub(crate) trait ImagePublisher {
    fn len(&self) -> Result<u64, String>;
    fn resize(&mut self, len: u64) -> Result<(), String>;
    fn read_at(&mut self, offset: u64, bytes: &mut [u8]) -> Result<usize, String>;
    fn write_at(&mut self, offset: u64, bytes: &[u8]) -> Result<usize, String>;
    fn flush(&mut self) -> Result<(), String>;
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct ImagePreparationError {
    operation: &'static str,
    detail: String,
    rollback: Option<String>,
}

impl ImagePreparationError {
    pub(crate) fn new(operation: &'static str, detail: impl Into<String>) -> Self {
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

pub(crate) fn prepare_file_image<P, A>(
    publisher: &mut P,
    configured_capacity: Option<u64>,
    default_capacity: u64,
    filesystem: FilesystemFormat,
    mut allocate: A,
) -> Result<Vec<u8>, ImagePreparationError>
where
    P: ImagePublisher,
    A: FnMut(u64) -> Result<Vec<u8>, ImagePreparationError>,
{
    let existing_len = publisher
        .len()
        .map_err(|detail| ImagePreparationError::new("inspect backing file", detail))?;
    let capacity = configured_capacity.unwrap_or(if existing_len == 0 {
        default_capacity
    } else {
        existing_len
    });
    if capacity == 0 || !capacity.is_multiple_of(512) {
        return Err(ImagePreparationError::new(
            "validate backing file",
            "capacity must be a positive multiple of 512 bytes",
        ));
    }

    let mut bytes = allocate(capacity)?;
    if existing_len != 0 {
        let bytes_to_read = existing_len.min(capacity);
        let bytes_to_read = usize::try_from(bytes_to_read).map_err(|_| {
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
                "backing file returned a short read",
            ));
        }
        if existing_len != capacity {
            publisher
                .resize(capacity)
                .map_err(|detail| ImagePreparationError::new("resize backing file", detail))?;
        }
        return Ok(bytes);
    }

    match filesystem {
        #[cfg(all(test, feature = "fs"))]
        FilesystemFormat::Unformatted => {}
        FilesystemFormat::Ext4 => format_ext4_image(&mut bytes)
            .map_err(|error| ImagePreparationError::new("format ext4 image", format!("{error}")))?,
    }
    publish_new_image(publisher, &bytes)?;
    Ok(bytes)
}

fn publish_new_image<P: ImagePublisher>(
    publisher: &mut P,
    bytes: &[u8],
) -> Result<(), ImagePreparationError> {
    let publish = publisher
        .resize(bytes.len() as u64)
        .map_err(|detail| ImagePreparationError::new("resize backing file", detail))
        .and_then(|()| {
            publisher
                .write_at(0, bytes)
                .map_err(|detail| ImagePreparationError::new("write backing file", detail))
        })
        .and_then(|written| {
            if written == bytes.len() {
                Ok(())
            } else {
                Err(ImagePreparationError::new(
                    "write backing file",
                    "backing file returned a short write",
                ))
            }
        })
        .and_then(|()| {
            publisher
                .flush()
                .map_err(|detail| ImagePreparationError::new("flush backing file", detail))
        });

    match publish {
        Ok(()) => Ok(()),
        Err(error) => {
            let rollback = publisher.resize(0).and_then(|()| publisher.flush()).err();
            Err(match rollback {
                Some(rollback) => error.with_rollback(rollback),
                None => error,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use axvmconfig::{GuestConfig, VirtualDeviceRequest};

    use super::{
        super::{device::prepare_file_backend_image, options::FilesystemFormat},
        *,
    };

    const TEST_IMAGE_CAPACITY: u64 = 64 * 1024 * 1024;
    const EXPLICIT_EXT4_CAPACITY: u64 = 128 * 1024 * 1024;
    const PRODUCTION_DEFAULT_CAPACITY: u64 = 2 * 1024 * 1024;
    const EXT4_MAGIC_OFFSET: usize = 1024 + 56;

    #[derive(Default)]
    struct MemoryPublisher {
        bytes: Vec<u8>,
        resize_calls: Vec<u64>,
        writes: usize,
        fail_resize_to: Option<u64>,
        fail_read: bool,
        fail_write: bool,
        fail_flush: bool,
        fail_next_flush: bool,
    }

    impl ImagePublisher for MemoryPublisher {
        fn len(&self) -> Result<u64, String> {
            Ok(self.bytes.len() as u64)
        }

        fn resize(&mut self, len: u64) -> Result<(), String> {
            self.resize_calls.push(len);
            if self.fail_resize_to == Some(len) {
                return Err(format!("resize to {len}"));
            }
            self.bytes.resize(len as usize, 0);
            Ok(())
        }

        fn read_at(&mut self, offset: u64, bytes: &mut [u8]) -> Result<usize, String> {
            if self.fail_read {
                return Err("read failed".into());
            }
            let start = offset as usize;
            let available = self.bytes.len().saturating_sub(start).min(bytes.len());
            bytes[..available].copy_from_slice(&self.bytes[start..start + available]);
            Ok(available)
        }

        fn write_at(&mut self, offset: u64, bytes: &[u8]) -> Result<usize, String> {
            self.writes += 1;
            if self.fail_write {
                return Err("write failed".into());
            }
            let start = offset as usize;
            self.bytes[start..start + bytes.len()].copy_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> Result<(), String> {
            if self.fail_flush || core::mem::take(&mut self.fail_next_flush) {
                Err("flush failed".into())
            } else {
                Ok(())
            }
        }
    }

    fn allocate(capacity: u64) -> Result<Vec<u8>, ImagePreparationError> {
        Ok(vec![0; capacity as usize])
    }

    #[test]
    fn formats_ext4_superblock_in_memory() {
        let mut image = vec![0_u8; 128 * 1024 * 1024];

        format_ext4_image(&mut image).expect("format image");

        let magic_offset = 1024 + 56;
        assert_eq!(
            u16::from_le_bytes([image[magic_offset], image[magic_offset + 1]]),
            rsext4::EXT4_SUPER_MAGIC
        );
    }

    #[test]
    fn rejects_out_of_range_block_access() {
        let mut bytes = [0_u8; BLOCK_SIZE];
        let mut image = Ext4Image::new(&mut bytes).expect("valid image");
        let mut block = [0_u8; BLOCK_SIZE];

        assert!(image.read(&mut block, SectorId::new(1), 1).is_err());
    }

    #[test]
    fn rejects_small_image_without_panicking() {
        let mut image = vec![0_u8; 1024 * 1024];
        assert!(format_ext4_image(&mut image).is_err());
    }

    #[test]
    fn empty_unformatted_file_is_zero_filled_and_published() {
        let mut publisher = MemoryPublisher::default();
        let image = prepare_file_image(
            &mut publisher,
            Some(TEST_IMAGE_CAPACITY),
            0,
            FilesystemFormat::Unformatted,
            allocate,
        )
        .expect("prepare unformatted image");

        assert_eq!(publisher.bytes, image);
        assert_eq!(publisher.writes, 1);
        assert!(image.iter().all(|byte| *byte == 0));
        assert_ne!(ext4_magic(&image), rsext4::EXT4_SUPER_MAGIC);
    }

    #[test]
    fn empty_ext4_file_is_formatted_and_published() {
        let mut publisher = MemoryPublisher::default();
        let image = prepare_file_image(
            &mut publisher,
            Some(TEST_IMAGE_CAPACITY),
            0,
            FilesystemFormat::Ext4,
            allocate,
        )
        .expect("prepare ext4 image");

        assert_eq!(publisher.bytes, image);
        assert_eq!(publisher.writes, 1);
        assert_eq!(ext4_magic(&image), rsext4::EXT4_SUPER_MAGIC);
    }

    #[test]
    fn nonempty_file_is_preserved_and_resized_without_formatting() {
        for filesystem in [FilesystemFormat::Unformatted, FilesystemFormat::Ext4] {
            let original = vec![0x5a; 4096];
            let mut publisher = MemoryPublisher {
                bytes: original.clone(),
                ..Default::default()
            };

            let image = prepare_file_image(&mut publisher, Some(8192), 0, filesystem, allocate)
                .expect("load image");

            assert_eq!(&image[..original.len()], original);
            assert_eq!(&publisher.bytes[..original.len()], original);
            assert_eq!(publisher.bytes.len(), 8192);
            assert_eq!(publisher.resize_calls, [8192]);
            assert_eq!(publisher.writes, 0);
            assert_ne!(ext4_magic(&image), rsext4::EXT4_SUPER_MAGIC);
        }
    }

    #[test]
    fn nonempty_read_failure_does_not_resize_the_file() {
        let original = vec![0x5a; 128 * 1024];

        for filesystem in [FilesystemFormat::Unformatted, FilesystemFormat::Ext4] {
            let mut publisher = MemoryPublisher {
                bytes: original.clone(),
                fail_read: true,
                ..Default::default()
            };

            let error = prepare_file_image(
                &mut publisher,
                Some(64 * 1024),
                2 * 1024 * 1024,
                filesystem,
                allocate,
            )
            .expect_err("read failure must abort preparation");

            assert_eq!(publisher.bytes, original);
            assert!(publisher.resize_calls.is_empty());
            assert_eq!(error.operation, "load backing file");
        }
    }

    #[test]
    fn initial_resize_failure_restores_empty_file_for_both_formats() {
        for filesystem in [FilesystemFormat::Unformatted, FilesystemFormat::Ext4] {
            let mut publisher = MemoryPublisher {
                fail_resize_to: Some(TEST_IMAGE_CAPACITY),
                ..Default::default()
            };

            let error = prepare_file_image(
                &mut publisher,
                Some(TEST_IMAGE_CAPACITY),
                0,
                filesystem,
                allocate,
            )
            .expect_err("initial resize must fail");

            assert_eq!(error.operation, "resize backing file");
            assert_eq!(publisher.bytes.len(), 0);
        }
    }

    #[test]
    fn write_failure_restores_empty_file_for_both_formats() {
        for filesystem in [FilesystemFormat::Unformatted, FilesystemFormat::Ext4] {
            let mut publisher = MemoryPublisher {
                fail_write: true,
                ..Default::default()
            };

            let error = prepare_file_image(
                &mut publisher,
                Some(TEST_IMAGE_CAPACITY),
                0,
                filesystem,
                allocate,
            )
            .expect_err("write must fail");

            assert_eq!(error.operation, "write backing file");
            assert_eq!(publisher.bytes.len(), 0);
        }
    }

    #[test]
    fn flush_failure_restores_empty_file_for_both_formats() {
        for filesystem in [FilesystemFormat::Unformatted, FilesystemFormat::Ext4] {
            let mut publisher = MemoryPublisher {
                fail_next_flush: true,
                ..Default::default()
            };

            let error = prepare_file_image(
                &mut publisher,
                Some(TEST_IMAGE_CAPACITY),
                0,
                filesystem,
                allocate,
            )
            .expect_err("flush must fail");

            assert_eq!(error.operation, "flush backing file");
            assert_eq!(publisher.bytes.len(), 0);
        }
    }

    #[test]
    fn rollback_resize_failure_reports_primary_error_for_both_formats() {
        for filesystem in [FilesystemFormat::Unformatted, FilesystemFormat::Ext4] {
            let mut publisher = MemoryPublisher {
                fail_write: true,
                fail_resize_to: Some(0),
                ..Default::default()
            };

            let error = prepare_file_image(
                &mut publisher,
                Some(TEST_IMAGE_CAPACITY),
                0,
                filesystem,
                allocate,
            )
            .expect_err("rollback resize must fail");

            assert_eq!(error.operation, "write backing file");
            assert!(error.to_string().contains("rollback failed: resize to 0"));
            assert_eq!(publisher.bytes.len(), TEST_IMAGE_CAPACITY as usize);
        }
    }

    #[test]
    fn rollback_flush_failure_reports_primary_error_for_both_formats() {
        for filesystem in [FilesystemFormat::Unformatted, FilesystemFormat::Ext4] {
            let mut publisher = MemoryPublisher {
                fail_write: true,
                fail_flush: true,
                ..Default::default()
            };

            let error = prepare_file_image(
                &mut publisher,
                Some(TEST_IMAGE_CAPACITY),
                0,
                filesystem,
                allocate,
            )
            .expect_err("rollback flush must fail");

            assert_eq!(error.operation, "write backing file");
            assert!(error.to_string().contains("rollback failed: flush failed"));
            assert_eq!(publisher.bytes.len(), 0);
        }
    }

    #[test]
    fn retry_after_successful_rollback_publishes_both_formats() {
        for filesystem in [FilesystemFormat::Unformatted, FilesystemFormat::Ext4] {
            let mut publisher = MemoryPublisher {
                fail_write: true,
                ..Default::default()
            };
            let first_error = prepare_file_image(
                &mut publisher,
                Some(TEST_IMAGE_CAPACITY),
                0,
                filesystem,
                allocate,
            )
            .expect_err("first publication must fail");
            assert_eq!(first_error.operation, "write backing file");
            assert_eq!(publisher.bytes.len(), 0);
            publisher.fail_write = false;

            let image = prepare_file_image(
                &mut publisher,
                Some(TEST_IMAGE_CAPACITY),
                0,
                filesystem,
                allocate,
            )
            .expect("retry image");

            assert_eq!(publisher.bytes, image);
            assert_eq!(publisher.bytes.len(), TEST_IMAGE_CAPACITY as usize);
        }
    }

    #[test]
    fn production_request_path_formats_ext4_when_filesystem_is_omitted_or_explicit() {
        let default_request = file_request("");
        let mut default_publisher = MemoryPublisher::default();
        let default_image = prepare_file_backend_image(
            &default_request,
            &mut default_publisher,
            TEST_IMAGE_CAPACITY,
            allocate,
        )
        .expect("prepare omitted-filesystem request");
        assert_eq!(ext4_magic(&default_image), rsext4::EXT4_SUPER_MAGIC);

        let ext4_request = file_request(r#"filesystem = "ext4""#);
        let mut ext4_publisher = MemoryPublisher::default();
        let ext4 = prepare_file_backend_image(
            &ext4_request,
            &mut ext4_publisher,
            TEST_IMAGE_CAPACITY,
            allocate,
        )
        .expect("prepare explicit-ext4 request");
        assert_eq!(ext4_magic(&ext4), rsext4::EXT4_SUPER_MAGIC);
    }

    #[test]
    fn production_request_path_uses_ext4_default_when_capacity_is_omitted() {
        let request = file_request_without_capacity(r#"filesystem = "ext4""#);
        let mut publisher = MemoryPublisher::default();

        let image = prepare_file_backend_image(
            &request,
            &mut publisher,
            PRODUCTION_DEFAULT_CAPACITY,
            allocate,
        )
        .expect("prepare default-capacity ext4 request");

        assert_eq!(image.len(), TEST_IMAGE_CAPACITY as usize);
        assert_eq!(publisher.bytes, image);
        assert_eq!(ext4_magic(&image), rsext4::EXT4_SUPER_MAGIC);
    }

    #[test]
    fn production_request_path_uses_ext4_defaults_when_capacity_and_filesystem_are_omitted() {
        let request = file_request_without_capacity("");
        let mut publisher = MemoryPublisher::default();

        let image = prepare_file_backend_image(
            &request,
            &mut publisher,
            PRODUCTION_DEFAULT_CAPACITY,
            allocate,
        )
        .expect("prepare default-capacity ext4 request");

        assert_eq!(image.len(), TEST_IMAGE_CAPACITY as usize);
        assert_eq!(publisher.bytes, image);
        assert_eq!(ext4_magic(&image), rsext4::EXT4_SUPER_MAGIC);
    }

    #[test]
    fn production_request_path_prefers_explicit_ext4_capacity() {
        let request = file_request_with_capacity("128MiB", r#"filesystem = "ext4""#);
        let mut publisher = MemoryPublisher::default();

        let image = prepare_file_backend_image(
            &request,
            &mut publisher,
            PRODUCTION_DEFAULT_CAPACITY,
            allocate,
        )
        .expect("prepare explicit-capacity ext4 request");

        assert_eq!(image.len(), EXPLICIT_EXT4_CAPACITY as usize);
        assert_eq!(publisher.bytes, image);
        assert_eq!(ext4_magic(&image), rsext4::EXT4_SUPER_MAGIC);
    }

    fn ext4_magic(image: &[u8]) -> u16 {
        u16::from_le_bytes([image[EXT4_MAGIC_OFFSET], image[EXT4_MAGIC_OFFSET + 1]])
    }

    fn file_request(filesystem: &str) -> VirtualDeviceRequest {
        file_request_with_capacity("64MiB", filesystem)
    }

    fn file_request_with_capacity(capacity: &str, filesystem: &str) -> VirtualDeviceRequest {
        let config = GuestConfig::from_toml(&format!(
            r#"
[devices]
[[devices.virtual]]
id = "data"
model = "virtio-blk"
path = "/tmp/data.img"
capacity = "{capacity}"
{filesystem}
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

    fn file_request_without_capacity(filesystem: &str) -> VirtualDeviceRequest {
        let config = GuestConfig::from_toml(&format!(
            r#"
[devices]
[[devices.virtual]]
id = "data"
model = "virtio-blk"
path = "/tmp/data.img"
{filesystem}
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
