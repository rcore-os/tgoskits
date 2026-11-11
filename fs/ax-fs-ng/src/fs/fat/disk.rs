use alloc::{boxed::Box, sync::Arc, vec};
use core::mem;

use crate::{
    BlockError as FsBlockError, BlockResult as FsBlockResult,
    block::{BlockRegion, FsBlockDevice, RegionBlockDevice},
    os::sync::Mutex,
};

fn take<'a>(buf: &mut &'a [u8], cnt: usize) -> &'a [u8] {
    let (first, rem) = buf.split_at(cnt);
    *buf = rem;
    first
}

fn take_mut<'a>(buf: &mut &'a mut [u8], cnt: usize) -> &'a mut [u8] {
    // use mem::take to circumvent lifetime issues
    let (first, rem) = mem::take(buf).split_at_mut(cnt);
    *buf = rem;
    first
}

/// A disk device with a cursor.
pub struct SeekableDisk {
    state: Arc<Mutex<SeekableDiskState>>,
}

#[derive(Clone)]
pub struct SeekableDiskFlusher {
    state: Arc<Mutex<SeekableDiskState>>,
}

struct SeekableDiskState {
    dev: RegionBlockDevice<Box<dyn FsBlockDevice>>,

    block_id: u64,
    offset: usize,
    block_size_log2: u8,

    read_buffer: Box<[u8]>,
    write_buffer: Box<[u8]>,
    /// Whether we have unsaved changes in the write buffer.
    ///
    /// It's guaranteed that when `offset == 0`, write_buffer_dirty is false.
    write_buffer_dirty: bool,
}

impl SeekableDisk {
    pub fn new(dev: Box<dyn FsBlockDevice>, region: BlockRegion) -> Self {
        assert!(dev.block_size().is_power_of_two());
        let block_size = dev.block_size();
        let block_size_log2 = block_size.trailing_zeros() as u8;
        let read_buffer = vec![0u8; block_size].into_boxed_slice();
        let write_buffer = vec![0u8; block_size].into_boxed_slice();
        Self {
            state: Arc::new(Mutex::new(SeekableDiskState {
                dev: RegionBlockDevice::new(dev, region),
                block_id: 0,
                offset: 0,
                block_size_log2,
                read_buffer,
                write_buffer,
                write_buffer_dirty: false,
            })),
        }
    }

    pub fn flusher(&self) -> SeekableDiskFlusher {
        SeekableDiskFlusher {
            state: self.state.clone(),
        }
    }

    /// Get the size of the disk.
    pub fn size(&self) -> u64 {
        self.state.lock().size()
    }

    /// Get the position of the cursor.
    pub fn position(&self) -> u64 {
        self.state.lock().position()
    }

    /// Set the position of the cursor.
    pub fn set_position(&mut self, pos: u64) -> FsBlockResult<()> {
        self.state.lock().set_position(pos)
    }

    /// Write all pending changes to the disk and issue a device flush.
    pub fn flush(&mut self) -> FsBlockResult<()> {
        self.state.lock().sync()
    }

    /// Read from the disk, returns the number of bytes read.
    pub fn read(&mut self, buf: &mut [u8]) -> FsBlockResult<usize> {
        self.state.lock().read(buf)
    }

    /// Write to the disk, returns the number of bytes written.
    pub fn write(&mut self, buf: &[u8]) -> FsBlockResult<usize> {
        self.state.lock().write(buf)
    }
}

impl Drop for SeekableDisk {
    fn drop(&mut self) {
        if let Err(error) = self.state.lock().sync() {
            error!("failed to flush FAT disk while dropping cursor: {error:?}");
        }
    }
}

impl SeekableDiskFlusher {
    /// Queries the retained backing authority without relying on VFS flags.
    pub(super) fn is_read_only(&self) -> bool {
        self.state.lock().dev.is_read_only()
    }

    pub fn flush(&self) -> FsBlockResult<()> {
        self.state.lock().sync()
    }
}

impl SeekableDiskState {
    fn size(&self) -> u64 {
        self.dev.num_blocks() << self.block_size_log2
    }

    fn block_size(&self) -> usize {
        1 << self.block_size_log2
    }

    fn position(&self) -> u64 {
        (self.block_id << self.block_size_log2) + self.offset as u64
    }

    fn set_position(&mut self, pos: u64) -> FsBlockResult<()> {
        let block_id = pos >> self.block_size_log2;
        let offset = pos as usize & (self.block_size() - 1);
        if self.write_buffer_dirty && block_id != self.block_id {
            self.flush_buffer()?;
        }
        self.block_id = block_id;
        self.offset = offset;
        Ok(())
    }

    fn flush_buffer(&mut self) -> FsBlockResult<()> {
        if self.write_buffer_dirty {
            self.dev.write_block(self.block_id, &self.write_buffer)?;
            self.write_buffer_dirty = false;
        }
        Ok(())
    }

    fn sync(&mut self) -> FsBlockResult<()> {
        self.flush_buffer()?;
        self.dev.flush()
    }

    fn read_partial(&mut self, buf: &mut &mut [u8]) -> FsBlockResult<usize> {
        if self.write_buffer_dirty {
            self.read_buffer.copy_from_slice(&self.write_buffer);
        } else {
            self.dev.read_block(self.block_id, &mut self.read_buffer)?;
        }

        let data = &self.read_buffer[self.offset..];
        let length = buf.len().min(data.len());
        take_mut(buf, length).copy_from_slice(&data[..length]);

        self.offset += length;
        if self.offset == self.block_size() {
            self.block_id += 1;
            self.offset = 0;
        }

        Ok(length)
    }

    /// Read from the disk, returns the number of bytes read.
    fn read(&mut self, mut buf: &mut [u8]) -> FsBlockResult<usize> {
        let mut read = 0;
        if self.offset != 0 {
            read += self.read_partial(&mut buf)?;
        }
        if buf.len() >= self.block_size() {
            self.flush_buffer()?;
            let blocks = buf.len() >> self.block_size_log2;
            let length = blocks << self.block_size_log2;
            self.dev
                .read_block(self.block_id, take_mut(&mut buf, length))?;
            read += length;

            self.block_id = self
                .block_id
                .checked_add(blocks as u64)
                .ok_or(FsBlockError::InvalidState)?;
        }
        if !buf.is_empty() {
            read += self.read_partial(&mut buf)?;
        }

        Ok(read)
    }

    fn write_partial(&mut self, buf: &mut &[u8]) -> FsBlockResult<usize> {
        if !self.write_buffer_dirty {
            self.dev.read_block(self.block_id, &mut self.write_buffer)?;
            self.write_buffer_dirty = true;
        }

        let data = &mut self.write_buffer[self.offset..];
        let length = buf.len().min(data.len());
        data[..length].copy_from_slice(take(buf, length));

        self.offset += length;
        if self.offset == self.block_size() {
            self.flush_buffer()?;
            self.block_id += 1;
            self.offset = 0;
        }

        Ok(length)
    }

    /// Write to the disk, returns the number of bytes written.
    fn write(&mut self, mut buf: &[u8]) -> FsBlockResult<usize> {
        let mut written = 0;
        if self.offset != 0 {
            written += self.write_partial(&mut buf)?;
        }
        if buf.len() >= self.block_size() {
            if self.write_buffer_dirty {
                self.flush_buffer()?;
            }
            let blocks = buf.len() >> self.block_size_log2;
            let length = blocks << self.block_size_log2;
            self.dev
                .write_block(self.block_id, take(&mut buf, length))?;
            written += length;

            self.block_id = self
                .block_id
                .checked_add(blocks as u64)
                .ok_or(FsBlockError::InvalidState)?;
        }
        if !buf.is_empty() {
            written += self.write_partial(&mut buf)?;
        }

        Ok(written)
    }
}

#[cfg(test)]
mod tests {
    use alloc::sync::Arc;
    use std::sync::Mutex;

    use super::*;

    struct MemoryBlockDevice {
        storage: Arc<Mutex<Vec<u8>>>,
        fail_flush: bool,
        read_only: bool,
    }

    impl FsBlockDevice for MemoryBlockDevice {
        fn name(&self) -> &str {
            "memory"
        }

        fn num_blocks(&self) -> u64 {
            (self.storage.lock().unwrap().len() / 512) as u64
        }

        fn block_size(&self) -> usize {
            512
        }

        #[cfg(feature = "ext4")]
        fn physical_block_size(&self) -> usize {
            512
        }

        fn is_read_only(&self) -> bool {
            self.read_only
        }

        #[cfg(feature = "ext4")]
        fn supports_flush(&self) -> bool {
            true
        }

        #[cfg(feature = "ext4")]
        fn supports_fua(&self) -> bool {
            false
        }

        fn read_block(&mut self, block_id: u64, buf: &mut [u8]) -> FsBlockResult<()> {
            let offset = block_id as usize * 512;
            buf.copy_from_slice(&self.storage.lock().unwrap()[offset..offset + buf.len()]);
            Ok(())
        }

        fn write_block(&mut self, block_id: u64, buf: &[u8]) -> FsBlockResult<()> {
            if self.read_only {
                return Err(FsBlockError::Unsupported);
            }
            let offset = block_id as usize * 512;
            self.storage.lock().unwrap()[offset..offset + buf.len()].copy_from_slice(buf);
            Ok(())
        }

        #[cfg(feature = "ext4")]
        fn write_block_fua(&mut self, _block_id: u64, _buf: &[u8]) -> FsBlockResult<()> {
            Err(FsBlockError::Unsupported)
        }

        fn flush(&mut self) -> FsBlockResult<()> {
            if self.fail_flush {
                Err(FsBlockError::Io)
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn dropping_disk_persists_pending_partial_write() {
        let storage = Arc::new(Mutex::new(vec![0; 512]));
        let device = MemoryBlockDevice {
            storage: storage.clone(),
            fail_flush: false,
            read_only: false,
        };
        let mut disk = SeekableDisk::new(Box::new(device), BlockRegion::from_num_blocks(1));

        disk.write(b"fat").expect("buffer partial FAT write");
        drop(disk);

        assert_eq!(&storage.lock().unwrap()[..3], b"fat");
    }

    #[test]
    fn flusher_persists_pending_write_and_propagates_device_error() {
        let storage = Arc::new(Mutex::new(vec![0; 512]));
        let device = MemoryBlockDevice {
            storage: storage.clone(),
            fail_flush: true,
            read_only: false,
        };
        let mut disk = SeekableDisk::new(Box::new(device), BlockRegion::from_num_blocks(1));
        let flusher = disk.flusher();
        disk.write(b"fat").expect("buffer partial FAT write");

        assert_eq!(flusher.flush(), Err(FsBlockError::Io));
        assert_eq!(&storage.lock().unwrap()[..3], b"fat");
    }

    #[test]
    fn fat_remount_respects_backing_write_authority() {
        use axfs_ng_vfs::VfsError;

        let storage = Arc::new(Mutex::new(vec![0; 2 * 1024 * 1024]));
        let region = BlockRegion::from_num_blocks(4096);
        let mut disk = SeekableDisk::new(
            Box::new(MemoryBlockDevice {
                storage: storage.clone(),
                fail_flush: false,
                read_only: false,
            }),
            region,
        );
        fatfs::format_volume(&mut disk, fatfs::FormatVolumeOptions::new()).unwrap();
        drop(disk);

        for read_only in [true, false] {
            let filesystem = super::super::FatFilesystem::new(
                Box::new(MemoryBlockDevice {
                    storage: storage.clone(),
                    fail_flush: false,
                    read_only,
                }),
                region,
                None,
            )
            .unwrap();
            // Native permissions, not just VFS flags, decide whether remount
            // may publish write access. A physical read-only backend has no
            // original writable authority to restore.
            assert_eq!(
                filesystem.root_dir().filesystem().remount_read_write(),
                if read_only {
                    Err(VfsError::PermissionDenied)
                } else {
                    Ok(())
                }
            );
            assert_eq!(filesystem.is_readonly(), read_only);
            let mount = axfs_ng_vfs::Mountpoint::new_root(&filesystem);
            // Clearing shared write protection must first succeed at the real
            // FAT/device boundary, and preserve flags when authority is absent.
            assert_eq!(
                mount.set_filesystem_readonly(false),
                if read_only {
                    Err(VfsError::PermissionDenied)
                } else {
                    Ok(())
                }
            );
            assert_eq!(mount.is_filesystem_readonly(), read_only);
            assert_eq!(mount.is_readonly(), read_only);
        }
    }

    fn formatted_fat() -> (axfs_ng_vfs::Filesystem, Arc<Mutex<Vec<u8>>>) {
        let storage = Arc::new(Mutex::new(vec![0; 2 * 1024 * 1024]));
        let region = BlockRegion::from_num_blocks(4096);
        let device = || {
            Box::new(MemoryBlockDevice {
                storage: storage.clone(),
                fail_flush: false,
                read_only: false,
            }) as Box<dyn FsBlockDevice>
        };
        let mut disk = SeekableDisk::new(device(), region);
        fatfs::format_volume(&mut disk, fatfs::FormatVolumeOptions::new()).unwrap();
        drop(disk);
        let filesystem = super::super::FatFilesystem::new(device(), region, None).unwrap();
        (filesystem, storage)
    }

    #[test]
    fn invalid_fat_rename_preserves_the_source_contents_and_open_handle() {
        use axfs_ng_vfs::{NameState, NodePermission, NodeType, VfsError};

        let (filesystem, _storage) = formatted_fat();
        let root = filesystem.root_dir();
        let directory = root.as_dir().unwrap();
        let source = axfs_ng_vfs::Location::new(
            axfs_ng_vfs::Mountpoint::new_root(&filesystem),
            root.clone(),
        )
        .create(
            "safe.txt",
            NodeType::RegularFile,
            NodePermission::default(),
            0,
            0,
            &axfs_ng_vfs::MutationCredentials {
                fsuid: 0,
                fsgid: 0,
                ..axfs_ng_vfs::MutationCredentials::root()
            },
        )
        .map(|location| location.entry().clone())
        .unwrap();
        let opened = source.as_file().unwrap();
        let contents = b"original FAT contents";
        assert_eq!(opened.write_at(contents, 0).unwrap(), contents.len());
        opened.sync(false).unwrap();
        let inode = source.inode();

        let mount = axfs_ng_vfs::Mountpoint::new_root(&filesystem);
        let location = mount.root_location();
        let result = location.rename(
            "safe.txt",
            &location,
            "bad:name",
            &axfs_ng_vfs::MutationCredentials::root(),
        );
        let mut old_contents = [0; 21];
        let old_read = opened.read_at(&mut old_contents, 0);
        directory.clear_cached_entries();
        let rediscovered = directory.lookup("safe.txt");

        assert_eq!(old_read, Ok(contents.len()));
        assert_eq!(&old_contents, contents);
        let rediscovered = rediscovered.expect("invalid rename must leave the source on disk");
        assert!(rediscovered.ptr_eq(&source));
        assert_eq!(rediscovered.inode(), inode);
        assert_eq!(source.name_state(), NameState::Linked);
        assert_eq!(source.name(), "safe.txt");
        assert!(source.parent().unwrap().ptr_eq(&root));
        assert_eq!(result, Err(VfsError::InvalidInput));
        assert!(matches!(
            directory.lookup("bad:name"),
            Err(VfsError::NotFound)
        ));
        assert_eq!(opened.write_at(b"still open", 0).unwrap(), 10);
        opened.sync(false).unwrap();
        let mut updated = [0; 10];
        assert_eq!(
            rediscovered
                .as_file()
                .unwrap()
                .read_at(&mut updated, 0)
                .unwrap(),
            updated.len()
        );
        assert_eq!(&updated, b"still open");
    }

    #[test]
    fn fat_unlink_preserves_active_identity_and_releases_closed_cache_owners() {
        use axfs_ng_vfs::{
            Mountpoint, MutationCredentials, NameState, NodePermission, NodeType, VfsError,
        };

        use crate::{
            file::{FileBackend, FileFlags, OpenOptions, OpenResult},
            highlevel::FsContext,
        };

        let (filesystem, _storage) = formatted_fat();
        let root = Mountpoint::new_root(&filesystem).root_location();
        let credentials = MutationCredentials::root();
        let parent = root
            .create(
                "parent",
                NodeType::Directory,
                NodePermission::default(),
                0,
                0,
                &credentials,
            )
            .unwrap();
        let context = FsContext::new(parent.clone());
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true);
        let file = options
            .open_with_credentials(&context, "data", &credentials)
            .and_then(OpenResult::into_file)
            .unwrap();
        assert!(matches!(
            file.access(FileFlags::READ).unwrap(),
            FileBackend::Cached(_)
        ));
        let contents = b"retained FAT contents";
        assert_eq!(file.write_at(contents.as_slice(), 0), Ok(contents.len()));
        file.sync(false).unwrap();
        root.entry().filesystem().flush().unwrap();

        let original = file.location().entry().clone();
        let key = original.key();
        assert_eq!(
            parent.unlink("data", false, &credentials),
            Err(VfsError::OperationNotSupported)
        );
        let current = parent.lookup_no_follow("data").unwrap();
        assert!(current.entry().ptr_eq(&original));
        assert_eq!(current.entry().key(), key);
        assert_eq!(original.name_state(), NameState::Linked);
        assert_eq!(original.name(), "data");
        assert!(original.parent().unwrap().ptr_eq(parent.entry()));
        let mut bytes = [0; 21];
        assert_eq!(file.read_at(&mut bytes[..], 0), Ok(contents.len()));
        assert_eq!(&bytes, contents);
        let mut native_file_present = false;
        parent
            .read_dir(
                axfs_ng_vfs::DirectoryCursor::START,
                &mut |name: &[u8], _, _, _| {
                    native_file_present |= name == b"data";
                    true
                },
            )
            .unwrap();
        assert!(native_file_present);
        assert!(
            root.lookup_no_follow("parent")
                .unwrap()
                .entry()
                .ptr_eq(parent.entry())
        );

        // Keep the positive and clean content caches intact. Only actual users
        // leave scope; unlink preparation must release eligible cache owners.
        drop(current);
        drop(original);
        drop(file);
        drop(context);
        // Cancelling an observed native target must not commit it or leave a
        // holder owned only by the cancelled round. The next preparation can
        // then make progress without weakening FAT's active-holder rule.
        let write = axfs_ng_vfs::PreparedWrite::acquire(&parent).unwrap();
        let prepared = parent.prepare_unlink("data").unwrap();
        let scope = axfs_ng_vfs::NamespaceOperationScope::acquire(&write).unwrap();
        let operation = scope.context();
        let observed = match scope
            .unlink(&operation, prepared, false, &credentials)
            .unwrap()
        {
            axfs_ng_vfs::NamespaceStart::Observed(observed) => observed,
            axfs_ng_vfs::NamespaceStart::Complete(_) => panic!("expected actual FAT observation"),
        };
        let cancelled = observed.cancel();
        assert_eq!(cancelled.state(), axfs_ng_vfs::NamespaceState::Unchanged);
        let cancelled = scope
            .finish()
            .unwrap_or_else(|_| panic!("wrong FAT admission"));
        assert_eq!(cancelled.state(), axfs_ng_vfs::NamespaceState::Unchanged);
        assert_eq!(cancelled.into_result(), Ok(()));
        assert!(parent.lookup_no_follow("data").is_ok());

        let prepared = parent.prepare_unlink("data").unwrap();
        let scope = axfs_ng_vfs::NamespaceOperationScope::acquire(&write).unwrap();
        let operation = scope.context();
        let observed = match scope
            .unlink(&operation, prepared, false, &credentials)
            .unwrap()
        {
            axfs_ng_vfs::NamespaceStart::Observed(observed) => observed,
            axfs_ng_vfs::NamespaceStart::Complete(_) => panic!("expected actual FAT observation"),
        };
        // A logical wrapper acquired *after* this real observation must not
        // cause a second active sample. The same locked driver resumes once.
        let wrapper = axfs_ng_vfs::Location::new(
            parent.mountpoint().clone(),
            observed.target().entry.clone(),
        );
        operation.retired().retire(wrapper);
        let _ = observed.resume();
        let completed = scope
            .finish()
            .unwrap_or_else(|_| panic!("wrong FAT admission"));
        assert_eq!(completed.state(), axfs_ng_vfs::NamespaceState::Applied);
        let removed_file = completed.into_result();
        drop(write);
        let file_absent = matches!(parent.lookup_no_follow("data"), Err(VfsError::NotFound));
        drop(parent);
        let removed_parent = root.unlink("parent", true, &credentials);
        assert_eq!((removed_file, removed_parent), (Ok(()), Ok(())));
        assert!(file_absent);
        assert!(matches!(
            root.lookup_no_follow("parent"),
            Err(VfsError::NotFound)
        ));
        let mut native_parent_present = false;
        root.read_dir(
            axfs_ng_vfs::DirectoryCursor::START,
            &mut |name: &[u8], _, _, _| {
                native_parent_present |= name == b"parent";
                true
            },
        )
        .unwrap();
        assert!(!native_parent_present);

        #[cfg(feature = "host-test")]
        assert_unlink_blocks_new_cache_readers(&root);
    }

    #[cfg(feature = "host-test")]
    fn assert_unlink_blocks_new_cache_readers(root: &axfs_ng_vfs::Location) {
        use axfs_ng_vfs::{
            MutationCredentials, NodePermission, NodeType, VfsError,
            testing::UnlinkPublicationProbe,
        };

        // No reader owns the target before the active sample. The only lookup
        // starts after the real attempt has published its canonical candidate.
        drop(
            root.create(
                "reader",
                NodeType::RegularFile,
                NodePermission::default(),
                0,
                0,
                &MutationCredentials::root(),
            )
            .unwrap(),
        );
        let probe = Arc::new(UnlinkPublicationProbe::default());
        let directory = root.entry().as_dir().unwrap();
        directory.set_unlink_probe(Some(probe.clone()));
        let writer_root = root.clone();
        let writer = std::thread::spawn(move || {
            writer_root.unlink("reader", false, &MutationCredentials::root())
        });
        probe.wait_for_publication();
        let reader_root = root.clone();
        let reader_probe = probe.clone();
        let reader = std::thread::spawn(move || {
            let result = reader_root.lookup_no_follow("reader");
            reader_probe.reader_completed();
            result
        });
        let blocked = probe.wait_for_reader();
        probe.resume();
        let removed = writer.join().unwrap();
        let observed = reader.join().unwrap();
        directory.set_unlink_probe(None);
        assert!(blocked, "lookup bypassed the held native filesystem gate");
        assert_eq!(removed, Ok(()));
        assert_eq!(observed.unwrap_err(), VfsError::NotFound);
    }

    #[test]
    fn fat_root_rediscovery_preserves_identity_and_releases_the_device_after_its_last_child() {
        use axfs_ng_vfs::{NodePermission, NodeType};

        let (filesystem, storage) = formatted_fat();
        let device_lifetime = Arc::downgrade(&storage);
        let first = filesystem.root_dir();
        let root_inode = first.inode();
        let root_identity = first.identity().cloned();
        let root_lifetime = first.downgrade();
        drop(first);
        let root_was_released = root_lifetime.upgrade().is_none();

        for _ in 0..3 {
            let root = filesystem.root_dir();
            let alias = filesystem.root_dir();
            assert!(root.ptr_eq(&alias));
            assert_eq!(root.inode(), root_inode);
            assert_eq!(root.identity(), root_identity.as_ref());
        }
        let root = filesystem.root_dir();
        let child = axfs_ng_vfs::Location::new(
            axfs_ng_vfs::Mountpoint::new_root(&filesystem),
            root.clone(),
        )
        .create(
            "active-child",
            NodeType::Directory,
            NodePermission::default(),
            0,
            0,
            &axfs_ng_vfs::MutationCredentials {
                fsuid: 0,
                fsgid: 0,
                ..axfs_ng_vfs::MutationCredentials::root()
            },
        )
        .map(|location| location.entry().clone())
        .unwrap();
        root.as_dir().unwrap().clear_cached_entries();
        drop(root);
        drop(filesystem);
        drop(storage);
        let device_alive_with_child = device_lifetime.upgrade().is_some();
        let child_is_usable = child.metadata().is_ok();
        drop(child);
        let device_was_released = device_lifetime.upgrade().is_none();

        assert!(
            root_was_released,
            "the filesystem must only weakly index its root"
        );
        assert!(device_alive_with_child && child_is_usable);
        assert!(
            device_was_released,
            "the last child and root holder must release the FAT device"
        );
    }
}
