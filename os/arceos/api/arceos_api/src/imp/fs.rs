use alloc::string::String;
use ax_errno::AxResult;
use ax_fs::{File, ReadDir};
use ax_fs_vfs::{Metadata, NodePermission, NodeType};
pub use ax_io::SeekFrom as AxSeekFrom;
use ax_io::prelude::*;

pub type AxFileType = NodeType;
pub type AxFilePerm = NodePermission;

/// File metadata exposed through the stable ArceOS API.
pub struct AxFileAttr(Metadata);

impl AxFileAttr {
    pub const fn file_type(&self) -> AxFileType {
        self.0.node_type
    }

    pub const fn is_dir(&self) -> bool {
        self.0.node_type.is_dir()
    }

    pub const fn is_file(&self) -> bool {
        self.0.node_type.is_file()
    }

    pub const fn perm(&self) -> AxFilePerm {
        self.0.mode
    }

    pub const fn size(&self) -> u64 {
        self.0.size
    }

    pub const fn blocks(&self) -> u64 {
        self.0.blocks
    }
}

/// A single directory entry exposed through the stable ArceOS API.
#[derive(Clone, Debug)]
pub struct AxDirEntry {
    name: String,
    entry_type: AxFileType,
}

impl Default for AxDirEntry {
    fn default() -> Self {
        Self {
            name: String::new(),
            entry_type: AxFileType::Unknown,
        }
    }
}

impl AxDirEntry {
    pub fn name_as_bytes(&self) -> &[u8] {
        self.name.as_bytes()
    }

    pub const fn entry_type(&self) -> AxFileType {
        self.entry_type
    }
}

/// Options and flags used when opening files and directories.
#[derive(Clone)]
pub struct AxOpenOptions(ax_fs::OpenOptions);

impl AxOpenOptions {
    pub const fn new() -> Self {
        Self(ax_fs::OpenOptions::new())
    }

    pub fn read(&mut self, read: bool) {
        self.0.read(read);
    }

    pub fn write(&mut self, write: bool) {
        self.0.write(write);
    }

    pub fn append(&mut self, append: bool) {
        self.0.append(append);
    }

    pub fn truncate(&mut self, truncate: bool) {
        self.0.truncate(truncate);
    }

    pub fn create(&mut self, create: bool) {
        self.0.create(create);
    }

    pub fn create_new(&mut self, create_new: bool) {
        self.0.create_new(create_new);
    }

    fn inner(&self) -> &ax_fs::OpenOptions {
        &self.0
    }
}

impl core::fmt::Debug for AxOpenOptions {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("AxOpenOptions").finish_non_exhaustive()
    }
}

impl Default for AxOpenOptions {
    fn default() -> Self {
        Self::new()
    }
}

/// A handle to an opened file.
pub struct AxFileHandle(File);

/// A handle to an opened directory.
pub struct AxDirHandle(ReadDir);

pub fn ax_open_file(path: &str, opts: &AxOpenOptions) -> AxResult<AxFileHandle> {
    let ctx = ax_fs::FS_CONTEXT.lock();
    let file = opts.inner().open(&ctx, path)?.into_file()?;
    Ok(AxFileHandle(file))
}

pub fn ax_open_dir(path: &str, opts: &AxOpenOptions) -> AxResult<AxDirHandle> {
    let mut opts = opts.clone();
    opts.0.directory(true);
    let ctx = ax_fs::FS_CONTEXT.lock();
    let dir = opts.0.open(&ctx, path)?.into_dir()?;
    Ok(AxDirHandle(ReadDir::new(dir)))
}

pub fn ax_read_file(file: &mut AxFileHandle, buf: &mut [u8]) -> AxResult<usize> {
    file.0.read(buf)
}

pub fn ax_read_file_at(file: &AxFileHandle, offset: u64, buf: &mut [u8]) -> AxResult<usize> {
    file.0.read_at(buf, offset)
}

pub fn ax_write_file(file: &mut AxFileHandle, buf: &[u8]) -> AxResult<usize> {
    file.0.write(buf)
}

pub fn ax_write_file_at(file: &AxFileHandle, offset: u64, buf: &[u8]) -> AxResult<usize> {
    file.0.write_at(buf, offset)
}

pub fn ax_truncate_file(file: &AxFileHandle, size: u64) -> AxResult {
    file.0.backend()?.set_len(size)
}

pub fn ax_flush_file(file: &AxFileHandle) -> AxResult {
    file.0.flush()
}

pub fn ax_seek_file(file: &mut AxFileHandle, pos: AxSeekFrom) -> AxResult<u64> {
    (&file.0).seek(pos)
}

pub fn ax_file_attr(file: &AxFileHandle) -> AxResult<AxFileAttr> {
    Ok(AxFileAttr(file.0.location().metadata()?))
}

pub fn ax_read_dir(dir: &mut AxDirHandle, dirents: &mut [AxDirEntry]) -> AxResult<usize> {
    let mut count = 0;
    for slot in dirents {
        let Some(entry) = dir.0.next().transpose()? else {
            break;
        };
        *slot = AxDirEntry {
            name: entry.name,
            entry_type: entry.node_type,
        };
        count += 1;
    }
    Ok(count)
}

pub fn ax_create_dir(path: &str) -> AxResult {
    ax_fs::FS_CONTEXT
        .lock()
        .create_dir(path, NodePermission::default())
        .map(|_| ())
}

pub fn ax_remove_dir(path: &str) -> AxResult {
    ax_fs::FS_CONTEXT.lock().remove_dir(path)
}

pub fn ax_remove_file(path: &str) -> AxResult {
    ax_fs::FS_CONTEXT.lock().remove_file(path)
}

pub fn ax_rename(old: &str, new: &str) -> AxResult {
    ax_fs::FS_CONTEXT.lock().rename(old, new)
}

pub fn ax_current_dir() -> AxResult<String> {
    ax_fs::FS_CONTEXT
        .lock()
        .current_dir()
        .absolute_path()
        .map(|path| path.as_str().into())
}

pub fn ax_set_current_dir(path: &str) -> AxResult {
    let mut ctx = ax_fs::FS_CONTEXT.lock();
    let loc = ctx.resolve(path)?;
    ctx.set_current_dir(loc)
}
