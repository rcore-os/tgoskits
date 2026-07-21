use alloc::{string::String, vec::Vec};

use ax_errno::{AxError, AxResult};
use ax_io::{Seek, SeekFrom};
use axfs_ng_vfs::{Metadata, NodePermission, NodeType};

use crate::highlevel::{File as CoreFile, OpenOptions as CoreOpenOptions, current_fs_context};

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
}

impl Default for DirEntry {
    fn default() -> Self {
        Self {
            name: String::new(),
            ty: FileType::Unknown,
        }
    }
}

impl DirEntry {
    pub const fn empty() -> Self {
        Self {
            name: String::new(),
            ty: FileType::Unknown,
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
    pub fn open(path: &str, opts: &OpenOptions) -> AxResult<Self> {
        let fs_context = current_fs_context();
        let inner = opts.to_core().open(&fs_context.lock(), path)?;
        Ok(Self {
            inner: inner.into_file()?,
        })
    }

    pub fn truncate(&self, size: u64) -> AxResult {
        self.inner.location().entry().as_file()?.set_len(size)?;
        Ok(())
    }

    pub fn read(&mut self, buf: &mut [u8]) -> AxResult<usize> {
        self.inner.read(buf)
    }

    pub fn read_at(&self, offset: u64, buf: &mut [u8]) -> AxResult<usize> {
        self.inner.read_at(buf, offset)
    }

    pub fn write(&mut self, buf: &[u8]) -> AxResult<usize> {
        self.inner.write(buf)
    }

    pub fn write_at(&self, offset: u64, buf: &[u8]) -> AxResult<usize> {
        self.inner.write_at(buf, offset)
    }

    pub fn flush(&self) -> AxResult {
        self.inner.sync(false)?;
        Ok(())
    }

    pub fn seek(&mut self, pos: SeekFrom) -> AxResult<u64> {
        (&self.inner).seek(pos)
    }

    pub fn get_attr(&self) -> AxResult<FileAttr> {
        self.inner.location().metadata()
    }
}

pub struct Directory {
    entries: Vec<DirEntry>,
    cursor: usize,
}

impl Directory {
    pub fn open_dir(path: &str, opts: &OpenOptions) -> AxResult<Self> {
        if !opts.read
            || opts.write
            || opts.append
            || opts.truncate
            || opts.create
            || opts.create_new
        {
            return Err(AxError::InvalidInput);
        }
        let entries = {
            let fs_context = current_fs_context();
            let ctx = fs_context.lock();
            let mut entries = Vec::new();
            for entry in ctx.read_dir(path)? {
                let entry = entry?;
                entries.push(DirEntry {
                    name: entry.name,
                    ty: entry.node_type,
                });
            }
            Ok::<_, AxError>(entries)
        }?;
        Ok(Self { entries, cursor: 0 })
    }

    pub fn read_dir(&mut self, dirents: &mut [DirEntry]) -> AxResult<usize> {
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
