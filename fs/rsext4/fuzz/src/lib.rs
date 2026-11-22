//! Shared pieces of the rsext4 fuzz targets: an in-memory block device over the fuzz input, a
//! fixed clock, and a bounded walk of a mounted filesystem.

use std::collections::BTreeSet;

use rsext4::{error::Ext4Result, *};

/// Sector size of the in-memory device. 512 bytes lets every ext4 block size mount.
pub const SECTOR_SIZE: u32 = 512;

/// Most inodes one walk visits, so that a large or cyclic image cannot stall an iteration.
const WALK_INODE_LIMIT: usize = 512;

/// Most bytes one walk reads from a single file.
const WALK_FILE_BYTE_LIMIT: u64 = 256 * 1024;

/// A block device backed by the fuzz input.
pub struct MemoryDevice {
    data: Vec<u8>,
    read_only: bool,
}

impl MemoryDevice {
    /// Wraps `image`, dropping a trailing partial sector.
    pub fn new(image: &[u8], read_only: bool) -> Self {
        let whole = image.len() - image.len() % SECTOR_SIZE as usize;
        Self {
            data: image[..whole].to_vec(),
            read_only,
        }
    }

    fn range(&self, sector: SectorId, bytes: usize) -> Ext4Result<core::ops::Range<usize>> {
        let start = sector
            .as_usize()?
            .checked_mul(SECTOR_SIZE as usize)
            .ok_or_else(Ext4Error::io)?;
        let end = start.checked_add(bytes).ok_or_else(Ext4Error::io)?;
        if end > self.data.len() {
            return Err(Ext4Error::io());
        }
        Ok(start..end)
    }
}

impl BlockIo for MemoryDevice {
    fn read(&mut self, buffer: &mut [u8], sector: SectorId, count: u32) -> Ext4Result<()> {
        let bytes = SECTOR_SIZE as usize * count as usize;
        if buffer.len() < bytes {
            return Err(Ext4Error::buffer_too_small(buffer.len(), bytes));
        }
        let range = self.range(sector, bytes)?;
        buffer[..bytes].copy_from_slice(&self.data[range]);
        Ok(())
    }

    fn write(&mut self, buffer: &[u8], sector: SectorId, count: u32) -> Ext4Result<()> {
        if self.read_only {
            return Err(Ext4Error::io());
        }
        let bytes = SECTOR_SIZE as usize * count as usize;
        if buffer.len() < bytes {
            return Err(Ext4Error::buffer_too_small(buffer.len(), bytes));
        }
        let range = self.range(sector, bytes)?;
        self.data[range].copy_from_slice(&buffer[..bytes]);
        Ok(())
    }

    fn flush(&mut self) -> Ext4Result<()> {
        Ok(())
    }

    fn geometry(&self) -> DeviceGeometry {
        DeviceGeometry::new(SECTOR_SIZE, (self.data.len() / SECTOR_SIZE as usize) as u64)
    }

    fn capabilities(&self) -> DeviceCapabilities {
        DeviceCapabilities {
            read_only: self.read_only,
            flush: true,
            ..DeviceCapabilities::default()
        }
    }
}

/// A clock that always reads the same time, so a run is reproducible.
pub struct FixedClock;

impl Clock for FixedClock {
    fn now(&self) -> Ext4Result<Ext4Timestamp> {
        Ok(Ext4Timestamp::new(1_600_000_000, 0))
    }
}

/// A mounted filesystem over a [`MemoryDevice`].
pub type MemoryFilesystem = Ext4<MemoryDevice, MountedServices<(), NoopObserver, ()>>;

/// Mounts `image`, or returns `None` when the crate refuses it.
pub fn mount(image: &[u8], options: MountOptions) -> Option<MemoryFilesystem> {
    let device = MemoryDevice::new(image, options.readonly);
    let services = MountServices::new(FixedClock, (), NoopObserver);
    Ext4::mount(device, services, options).ok()
}

/// Visits every directory reachable from the root, up to [`WALK_INODE_LIMIT`] inodes, and calls
/// `visit` with each directory and the entries read from it. Errors end the branch they occur on.
pub fn walk_directories(
    filesystem: &mut MemoryFilesystem,
    mut visit: impl FnMut(&mut MemoryFilesystem, InodeNumber, &[DirectoryEntry]),
) {
    let mut seen = BTreeSet::new();
    let mut pending = vec![filesystem.root_inode()];
    while let Some(directory) = pending.pop() {
        if !seen.insert(directory.raw()) || seen.len() > WALK_INODE_LIMIT {
            continue;
        }
        let mut entries = Vec::new();
        let mut cursor = DirectoryCursor::Start;
        while entries.len() < WALK_INODE_LIMIT {
            let Ok(batch) = filesystem.read_directory(directory, cursor, 64) else {
                break;
            };
            let Some(last) = batch.last() else {
                break;
            };
            cursor = last.next_cursor;
            entries.extend(batch);
            if cursor == DirectoryCursor::End {
                break;
            }
        }
        visit(filesystem, directory, &entries);
        for entry in &entries {
            if entry.name != b"."
                && entry.name != b".."
                && entry.file_type == DirectoryEntryType::Directory
            {
                pending.push(entry.inode);
            }
        }
    }
}

/// Reads what the walk reaches of one inode: its metadata, its extents, its extended
/// attributes, and up to [`WALK_FILE_BYTE_LIMIT`] bytes of its contents.
pub fn read_inode_fully(filesystem: &mut MemoryFilesystem, number: InodeNumber) {
    let Ok(info) = filesystem.inode(number) else {
        return;
    };
    let _ = filesystem.inode_extents(number, 0, u64::MAX / 2, FileExtentTarget::Data, 1024);
    if let Ok(names) = filesystem.list_xattrs(number) {
        for name in names.iter().take(64) {
            let _ = filesystem.get_xattr(number, name.namespace, &name.name);
        }
    }
    if matches!(
        info.file_type(),
        DirectoryEntryType::RegularFile | DirectoryEntryType::Symlink
    ) {
        let mut buffer = vec![0; 16 * 1024];
        let mut offset = 0;
        while offset < WALK_FILE_BYTE_LIMIT {
            match filesystem.read_inode(number, offset, &mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(read) => offset += read as u64,
            }
        }
    }
}
