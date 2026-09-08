//! Bounded metadata validation of existing filesystem images.

use core::fmt;
#[cfg(test)]
use std::vec::Vec;
use std::{format, string::String};

#[cfg(test)]
use rsext4::{
    BLOCK_SIZE, BlockIo, DeviceCapabilities, DeviceGeometry, Ext4Timestamp, Jbd2Dev, SectorId,
    error::{Ext4Error, Ext4Result},
};
use rsext4::{SUPERBLOCK_OFFSET, SUPERBLOCK_SIZE, endian::DiskFormat, superblock::Ext4Superblock};

use super::options::FilesystemFormat;

#[cfg(test)]
struct Ext4Image<'a> {
    bytes: &'a mut [u8],
}

#[cfg(test)]
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

#[cfg(test)]
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

#[cfg(test)]
impl rsext4::Clock for Ext4Image<'_> {
    fn now(&self) -> Ext4Result<Ext4Timestamp> {
        Ok(Ext4Timestamp::UNIX_EPOCH)
    }
}

fn validate_ext4_image(raw: &[u8], image_len: u64) -> Result<(), &'static str> {
    let superblock = Ext4Superblock::from_disk_bytes(raw);
    if !superblock.is_valid() {
        return Err("ext4 superblock magic is invalid");
    }
    superblock
        .validate_geometry()
        .map_err(|_| "ext4 superblock geometry is invalid")?;
    let superblock = superblock
        .verify_superblock()
        .map_err(|_| "ext4 superblock checksum is invalid")?;
    let filesystem_bytes = superblock
        .blocks_count()
        .checked_mul(superblock.block_size())
        .ok_or("ext4 filesystem size overflowed")?;
    if filesystem_bytes > image_len {
        return Err("ext4 filesystem exceeds the backing file length");
    }
    Ok(())
}

pub(crate) trait ImageReader {
    fn len(&self) -> Result<u64, String>;
    fn read_at(&mut self, offset: u64, bytes: &mut [u8]) -> Result<usize, String>;
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct ImagePreparationError {
    operation: &'static str,
    detail: String,
}

impl ImagePreparationError {
    pub(crate) fn new(operation: &'static str, detail: impl Into<String>) -> Self {
        Self {
            operation,
            detail: detail.into(),
        }
    }
}

impl fmt::Display for ImagePreparationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.operation, self.detail)
    }
}

pub(crate) fn inspect_file_image<R: ImageReader>(
    reader: &mut R,
    configured_capacity: Option<u64>,
    filesystem: FilesystemFormat,
) -> Result<u64, ImagePreparationError> {
    let existing_len = reader
        .len()
        .map_err(|detail| ImagePreparationError::new("inspect backing file", detail))?;
    if existing_len == 0 {
        return Err(ImagePreparationError::new(
            "validate backing file",
            "backing file must contain an existing filesystem image",
        ));
    }
    if !existing_len.is_multiple_of(512) {
        return Err(ImagePreparationError::new(
            "validate backing file",
            "backing file length must be a multiple of 512 bytes",
        ));
    }
    if let Some(configured_capacity) = configured_capacity
        && configured_capacity != existing_len
    {
        return Err(ImagePreparationError::new(
            "validate backing file",
            format!(
                "configured capacity {configured_capacity} does not match backing file length \
                 {existing_len}"
            ),
        ));
    }

    if existing_len < SUPERBLOCK_OFFSET + SUPERBLOCK_SIZE as u64 {
        return Err(ImagePreparationError::new(
            "validate ext4 image",
            "backing file is too small to contain an ext4 superblock",
        ));
    }
    let mut bytes = [0; SUPERBLOCK_SIZE];
    let read = reader
        .read_at(SUPERBLOCK_OFFSET, &mut bytes)
        .map_err(|detail| ImagePreparationError::new("load backing file", detail))?;
    if read != bytes.len() {
        return Err(ImagePreparationError::new(
            "load backing file",
            "backing file returned a short read",
        ));
    }

    match filesystem {
        FilesystemFormat::Ext4 => validate_ext4_image(&bytes, existing_len)
            .map_err(|error| ImagePreparationError::new("validate ext4 image", error))?,
    }
    Ok(existing_len)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_IMAGE_CAPACITY: u64 = 64 * 1024 * 1024;

    #[derive(Default)]
    struct MemoryReader {
        bytes: Vec<u8>,
        fail_read: bool,
        short_read: bool,
    }

    impl ImageReader for MemoryReader {
        fn len(&self) -> Result<u64, String> {
            Ok(self.bytes.len() as u64)
        }

        fn read_at(&mut self, offset: u64, bytes: &mut [u8]) -> Result<usize, String> {
            if self.fail_read {
                return Err("read failed".into());
            }
            let start = offset as usize;
            let available = self.bytes.len().saturating_sub(start).min(bytes.len());
            let read = if self.short_read {
                available.saturating_sub(1)
            } else {
                available
            };
            bytes[..read].copy_from_slice(&self.bytes[start..start + read]);
            Ok(read)
        }
    }

    fn ext4_image() -> Vec<u8> {
        let mut bytes = vec![0_u8; TEST_IMAGE_CAPACITY as usize];
        {
            let image = Ext4Image::new(&mut bytes).expect("valid test image geometry");
            let mut device = Jbd2Dev::initial_jbd2dev(0, image, true);
            rsext4::mkfs(&mut device).expect("format test fixture");
        }
        bytes
    }

    #[test]
    fn large_image_initialization_reads_only_superblock() {
        struct LargeImage(Vec<u8>);
        impl ImageReader for LargeImage {
            fn len(&self) -> Result<u64, String> {
                Ok(1 << 40)
            }
            fn read_at(&mut self, offset: u64, bytes: &mut [u8]) -> Result<usize, String> {
                assert_eq!(offset, SUPERBLOCK_OFFSET);
                assert_eq!(bytes.len(), SUPERBLOCK_SIZE);
                bytes.copy_from_slice(&self.0[offset as usize..offset as usize + bytes.len()]);
                Ok(bytes.len())
            }
        }
        let mut reader = LargeImage(ext4_image());
        assert_eq!(
            inspect_file_image(&mut reader, None, FilesystemFormat::Ext4)
                .expect("inspect a large image with bounded memory"),
            1 << 40
        );
    }

    #[test]
    fn existing_ext4_image_is_loaded_without_modification() {
        let original = ext4_image();
        let mut reader = MemoryReader {
            bytes: original.clone(),
            ..Default::default()
        };

        let capacity = inspect_file_image(
            &mut reader,
            Some(TEST_IMAGE_CAPACITY),
            FilesystemFormat::Ext4,
        )
        .expect("load existing ext4 image");

        assert_eq!(capacity, TEST_IMAGE_CAPACITY);
        assert_eq!(reader.bytes, original);
    }

    #[test]
    fn empty_ext4_file_is_rejected() {
        let mut reader = MemoryReader::default();

        let error = inspect_file_image(&mut reader, None, FilesystemFormat::Ext4)
            .expect_err("an empty backing file must be rejected");

        assert_eq!(error.operation, "validate backing file");
    }

    #[test]
    fn non_ext4_backing_file_is_rejected() {
        let original = vec![0x5a; BLOCK_SIZE];
        let mut reader = MemoryReader {
            bytes: original.clone(),
            ..Default::default()
        };

        let error = inspect_file_image(&mut reader, None, FilesystemFormat::Ext4)
            .expect_err("a non-ext4 backing file must be rejected");

        assert_eq!(error.operation, "validate ext4 image");
        assert_eq!(reader.bytes, original);
    }

    #[test]
    fn configured_capacity_must_match_existing_image() {
        let mut reader = MemoryReader {
            bytes: ext4_image(),
            ..Default::default()
        };

        let error = inspect_file_image(
            &mut reader,
            Some(TEST_IMAGE_CAPACITY + 512),
            FilesystemFormat::Ext4,
        )
        .expect_err("capacity mismatch must be rejected");

        assert_eq!(error.operation, "validate backing file");
        assert_eq!(reader.bytes.len(), TEST_IMAGE_CAPACITY as usize);
    }

    #[test]
    fn backing_file_length_must_be_sector_aligned() {
        let mut reader = MemoryReader {
            bytes: vec![0; 513],
            ..Default::default()
        };

        let error = inspect_file_image(&mut reader, None, FilesystemFormat::Ext4)
            .expect_err("unaligned backing file must be rejected");

        assert_eq!(error.operation, "validate backing file");
    }

    #[test]
    fn read_failure_is_reported_without_modifying_the_image() {
        let original = ext4_image();
        let mut reader = MemoryReader {
            bytes: original.clone(),
            fail_read: true,
            ..Default::default()
        };

        let error = inspect_file_image(&mut reader, None, FilesystemFormat::Ext4)
            .expect_err("read failure must abort loading");

        assert_eq!(error.operation, "load backing file");
        assert_eq!(reader.bytes, original);
    }

    #[test]
    fn short_read_is_rejected() {
        let mut reader = MemoryReader {
            bytes: ext4_image(),
            short_read: true,
            ..Default::default()
        };

        let error = inspect_file_image(&mut reader, None, FilesystemFormat::Ext4)
            .expect_err("short read must be rejected");

        assert_eq!(error.operation, "load backing file");
    }
}
