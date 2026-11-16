use alloc::{string::String, vec::Vec};
use core::time::Duration;

use ax_io::{Seek, SeekFrom};
use axfs_ng_vfs::{
    FileRangeOperation, Location, Metadata, MetadataUpdate, NodePermission, NodeType,
    PreallocationMode, VfsError, VfsResult,
};

use crate::{
    fs_core::ReadDir,
    highlevel::{File as CoreFile, OpenOptions as CoreOpenOptions, OpenResult, current_fs_context},
    io_error_to_vfs_error,
};

pub type FileType = NodeType;
pub type FilePerm = NodePermission;
pub type FileAttr = Metadata;

pub trait FileTypeExt {
    fn is_dir(&self) -> bool;
    fn is_file(&self) -> bool;
    fn is_symlink(&self) -> bool;
    fn is_char_device(&self) -> bool;
    fn is_block_device(&self) -> bool;
    fn is_fifo(&self) -> bool;
    fn is_socket(&self) -> bool;
}

impl FileTypeExt for FileType {
    fn is_dir(&self) -> bool {
        matches!(self, FileType::Directory)
    }

    fn is_file(&self) -> bool {
        matches!(self, FileType::RegularFile)
    }

    fn is_symlink(&self) -> bool {
        matches!(self, FileType::Symlink)
    }

    fn is_char_device(&self) -> bool {
        matches!(self, FileType::CharacterDevice)
    }

    fn is_block_device(&self) -> bool {
        matches!(self, FileType::BlockDevice)
    }

    fn is_fifo(&self) -> bool {
        matches!(self, FileType::Fifo)
    }

    fn is_socket(&self) -> bool {
        matches!(self, FileType::Socket)
    }
}

pub trait FilePermExt {
    fn mode(&self) -> u32;
}

impl FilePermExt for FilePerm {
    fn mode(&self) -> u32 {
        self.bits() as u32
    }
}

#[derive(Clone, Debug)]
pub struct DirEntry {
    name: String,
    ty: FileType,
    ino: u64,
    next_offset: u64,
}

impl Default for DirEntry {
    fn default() -> Self {
        Self {
            name: String::new(),
            ty: FileType::Unknown,
            ino: 0,
            next_offset: 0,
        }
    }
}

impl DirEntry {
    pub const fn empty() -> Self {
        Self {
            name: String::new(),
            ty: FileType::Unknown,
            ino: 0,
            next_offset: 0,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn name_as_bytes(&self) -> &[u8] {
        self.name.as_bytes()
    }

    pub const fn entry_type(&self) -> FileType {
        self.ty
    }

    pub const fn inode(&self) -> u64 {
        self.ino
    }

    /// Linux-visible position of the next directory entry.
    pub const fn next_offset(&self) -> u64 {
        self.next_offset
    }
}

#[derive(Clone, Debug)]
pub struct OpenOptions {
    read: bool,
    write: bool,
    append: bool,
    truncate: bool,
    create: bool,
    create_new: bool,
}

impl Default for OpenOptions {
    fn default() -> Self {
        Self::new()
    }
}

impl OpenOptions {
    pub const fn new() -> Self {
        Self {
            read: false,
            write: false,
            append: false,
            truncate: false,
            create: false,
            create_new: false,
        }
    }

    pub fn read(&mut self, read: bool) {
        self.read = read;
    }

    pub fn write(&mut self, write: bool) {
        self.write = write;
    }

    pub fn append(&mut self, append: bool) {
        self.append = append;
    }

    pub fn truncate(&mut self, truncate: bool) {
        self.truncate = truncate;
    }

    pub fn create(&mut self, create: bool) {
        self.create = create;
    }

    pub fn create_new(&mut self, create_new: bool) {
        self.create_new = create_new;
    }

    fn to_core(&self) -> CoreOpenOptions {
        let mut options = CoreOpenOptions::new();
        options
            .read(self.read)
            .write(self.write)
            .append(self.append)
            .truncate(self.truncate)
            .create(self.create)
            .create_new(self.create_new);
        options
    }
}

pub struct File {
    inner: CoreFile,
}

impl File {
    pub fn open(
        path: &str,
        opts: &OpenOptions,
        credentials: &axfs_ng_vfs::MutationCredentials<'_>,
    ) -> VfsResult<Self> {
        let fs_context = current_fs_context();
        let inner = opts.to_core().open(&fs_context.lock(), path, credentials)?;
        Ok(Self {
            inner: inner.into_file()?,
        })
    }

    pub fn truncate(&self, size: u64) -> VfsResult {
        self.inner.set_len(size)?;
        Ok(())
    }

    pub fn preallocate(&self, offset: u64, len: u64, mode: PreallocationMode) -> VfsResult {
        self.inner.preallocate(offset, len, mode)
    }

    pub fn operate_range(&self, offset: u64, len: u64, operation: FileRangeOperation) -> VfsResult {
        self.inner.operate_range(offset, len, operation)
    }

    pub fn read(&mut self, buf: &mut [u8]) -> VfsResult<usize> {
        self.inner.read(buf).map_err(io_error_to_vfs_error)
    }

    pub fn read_at(&self, offset: u64, buf: &mut [u8]) -> VfsResult<usize> {
        self.inner.read_at(buf, offset)
    }

    pub fn write(&mut self, buf: &[u8]) -> VfsResult<usize> {
        self.inner.write(buf).map_err(io_error_to_vfs_error)
    }

    pub fn write_at(&self, offset: u64, buf: &[u8]) -> VfsResult<usize> {
        self.inner.write_at(buf, offset)
    }

    pub fn flush(&self) -> VfsResult {
        self.inner.sync(false)?;
        Ok(())
    }

    pub fn seek(&mut self, pos: SeekFrom) -> VfsResult<u64> {
        (&self.inner).seek(pos).map_err(io_error_to_vfs_error)
    }

    pub fn get_attr(&self) -> VfsResult<FileAttr> {
        self.inner.location().metadata()
    }

    /// Updates the access and modification times selected by non-`None` values.
    ///
    /// # Errors
    ///
    /// Returns the underlying filesystem error when metadata cannot be updated.
    pub fn set_times(&self, atime: Option<Duration>, mtime: Option<Duration>) -> VfsResult {
        self.inner.location().update_metadata(MetadataUpdate {
            atime,
            mtime,
            ..MetadataUpdate::default()
        })?;
        Ok(())
    }
}

/// Result of opening a path through [`open_at`].
pub enum OpenEntry {
    /// Opened regular file or character/block/FIFO node.
    File(File),
    /// Opened directory handle.
    Directory(Directory),
}

/// Immutable, cloneable handle to an open VFS directory.
#[derive(Clone)]
pub struct DirHandle(Location);

impl DirHandle {
    pub(crate) fn location(&self) -> &Location {
        &self.0
    }
}

/// Opens a file or directory relative to `dir` (or the current working directory when `None`).
pub fn open_at(
    dir: Option<&DirHandle>,
    path: &str,
    opts: &OpenOptions,
    directory_only: bool,
    credentials: &axfs_ng_vfs::MutationCredentials<'_>,
) -> VfsResult<OpenEntry> {
    let mut core_opts = opts.to_core();
    core_opts.directory(directory_only);
    let result =
        crate::api::with_at_context(dir, path, |ctx| core_opts.open(ctx, path, credentials))?;
    match result {
        OpenResult::File(inner) => Ok(OpenEntry::File(File { inner })),
        OpenResult::Dir(location) => Ok(OpenEntry::Directory(Directory::from_location(location)?)),
    }
}

pub struct Directory {
    location: Location,
    entries: Vec<DirEntry>,
    cursor: usize,
}

impl Directory {
    pub fn open_dir(path: &str, opts: &OpenOptions) -> VfsResult<Self> {
        if !opts.read
            || opts.write
            || opts.append
            || opts.truncate
            || opts.create
            || opts.create_new
        {
            return Err(VfsError::InvalidInput);
        }
        let location = current_fs_context().lock().resolve(path)?;
        Self::from_location(location)
    }

    pub(crate) fn from_location(location: Location) -> VfsResult<Self> {
        location.check_is_dir()?;
        let mut entries = Vec::new();
        for entry in ReadDir::from_location(location.clone())? {
            let entry = entry?;
            entries.push(DirEntry {
                name: entry.name,
                ty: entry.node_type,
                ino: entry.ino,
                next_offset: entry.offset,
            });
        }
        Ok(Self {
            location,
            entries,
            cursor: 0,
        })
    }

    /// Returns metadata for the underlying directory object.
    pub fn get_attr(&self) -> VfsResult<FileAttr> {
        self.location.metadata()
    }

    /// Returns a cloneable handle to the underlying VFS directory location.
    pub fn dir_handle(&self) -> DirHandle {
        DirHandle(self.location.clone())
    }

    pub fn read_dir(&mut self, dirents: &mut [DirEntry]) -> VfsResult<usize> {
        let mut count = 0;
        for slot in dirents.iter_mut() {
            let Some(entry) = self.entries.get(self.cursor).cloned() else {
                break;
            };
            *slot = entry;
            self.cursor += 1;
            count += 1;
        }
        Ok(count)
    }

    /// Returns the next entry without advancing the open-directory position.
    pub fn peek_dir_entry(&self) -> Option<&DirEntry> {
        self.entries.get(self.cursor)
    }

    /// Commits one successfully consumed entry.
    pub fn advance_dir_entry(&mut self) {
        if self.cursor < self.entries.len() {
            self.cursor += 1;
        }
    }

    /// Seeks the materialized directory view by its visible directory cookie.
    pub fn seek(&mut self, pos: SeekFrom) -> VfsResult<u64> {
        let current = self.current_offset();
        let end = self.entries.last().map_or(0, DirEntry::next_offset);
        let target = match pos {
            SeekFrom::Start(offset) => offset,
            SeekFrom::Current(delta) => current
                .checked_add_signed(delta)
                .ok_or(VfsError::InvalidInput)?,
            SeekFrom::End(delta) => end
                .checked_add_signed(delta)
                .ok_or(VfsError::InvalidInput)?,
        };
        self.cursor = if target == 0 {
            0
        } else {
            (0..self.entries.len())
                .find(|&index| {
                    let current = if index == 0 {
                        0
                    } else {
                        self.entries[index - 1].next_offset
                    };
                    current >= target
                })
                .unwrap_or(self.entries.len())
        };
        Ok(target)
    }

    fn current_offset(&self) -> u64 {
        self.cursor
            .checked_sub(1)
            .and_then(|index| self.entries.get(index))
            .map_or(0, DirEntry::next_offset)
    }
}

pub trait FileAttrExt {
    fn file_type(&self) -> FileType;
    fn perm(&self) -> FilePerm;
    fn size(&self) -> u64;
    fn blocks(&self) -> u64;
}

impl FileAttrExt for FileAttr {
    fn file_type(&self) -> FileType {
        self.node_type
    }

    fn perm(&self) -> FilePerm {
        self.mode
    }

    fn size(&self) -> u64 {
        self.size
    }

    fn blocks(&self) -> u64 {
        self.blocks
    }
}

#[cfg(test)]
mod directory_tests {
    use alloc::vec;

    use super::*;

    fn entry(name: &str, next_offset: u64) -> DirEntry {
        DirEntry {
            name: name.into(),
            ty: FileType::RegularFile,
            ino: 1,
            next_offset,
        }
    }

    fn test_location() -> Location {
        axfs_ng_vfs::Mountpoint::new_root(&crate::MemoryFs::new()).root_location()
    }

    #[test]
    fn peek_does_not_advance_until_output_is_committed() {
        let mut directory = Directory {
            location: test_location(),
            entries: vec![entry("first", 1), entry("second", 2)],
            cursor: 0,
        };

        assert_eq!(
            directory.peek_dir_entry().map(DirEntry::name),
            Some("first")
        );
        assert_eq!(
            directory.peek_dir_entry().map(DirEntry::name),
            Some("first")
        );
        directory.advance_dir_entry();
        assert_eq!(
            directory.peek_dir_entry().map(DirEntry::name),
            Some("second")
        );
    }

    #[test]
    fn seek_to_shared_htree_cookie_restarts_the_collision_chain() {
        let mut directory = Directory {
            location: test_location(),
            entries: vec![
                entry("before", 10),
                entry("collision-a", 10),
                entry("collision-b", 20),
                entry("after", 30),
            ],
            cursor: 4,
        };

        assert_eq!(directory.seek(SeekFrom::Start(10)), Ok(10));
        assert_eq!(
            directory.peek_dir_entry().map(DirEntry::name),
            Some("collision-a")
        );
        directory.advance_dir_entry();
        assert_eq!(
            directory.peek_dir_entry().map(DirEntry::name),
            Some("collision-b")
        );
    }
}
