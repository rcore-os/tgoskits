use alloc::{borrow::Cow, string::String, sync::Arc, vec::Vec};
use core::{any::Any, cmp::Ordering, task::Context};

use ax_sync::PiMutex;
use axfs_ng_vfs::{
    FileNodeOps, FilesystemOps, FsIoEvents, FsPollable, Metadata, MetadataUpdate, NodeFlags,
    NodeOps, NodePermission, NodeType, VfsError, VfsResult,
};
use axpoll::{IoEvents, Pollable};
use inherit_methods_macro::inherit_methods;

use super::fs::{SimpleFs, SimpleFsNode};

fn fs_events_to_io(events: FsIoEvents) -> IoEvents {
    IoEvents::from_bits_truncate(events.bits())
}

fn io_events_to_fs(events: IoEvents) -> FsIoEvents {
    FsIoEvents::from_bits_truncate(events.bits())
}

/// Operations for a simple file.
pub trait SimpleFileOps: Send + Sync + 'static {
    /// Reads all content in the file.
    fn read_all(&self) -> VfsResult<Cow<'_, [u8]>>;
    /// Replaces the file's content with `data`.
    fn write_all(&self, _data: &[u8]) -> VfsResult<()> {
        Err(VfsError::BadFileDescriptor)
    }
}

/// Type representing operation applied to a simple file.
pub enum SimpleFileOperation<'a> {
    /// Reading the file's content
    Read,
    /// Replacing the file's content
    Write(&'a [u8]),
}

/// A wrapper that implements [`SimpleFileOps`] for `Fn(SimpleFileOperation) ->
/// VfsResult<Option<impl Into<Vec<u8>>>>`.
pub struct RwFile<F>(F);

impl<F, R> RwFile<F>
where
    F: Fn(SimpleFileOperation) -> VfsResult<Option<R>> + Send + Sync,
    R: Into<Vec<u8>>,
{
    /// Creates a new `RwFile`.
    pub fn new(imp: F) -> Self {
        Self(imp)
    }
}

impl<F, R> SimpleFileOps for RwFile<F>
where
    F: Fn(SimpleFileOperation) -> VfsResult<Option<R>> + Send + Sync + 'static,
    R: Into<Vec<u8>>,
{
    fn read_all(&self) -> VfsResult<Cow<'_, [u8]>> {
        (self.0)(SimpleFileOperation::Read).map(|it| Cow::Owned(it.unwrap().into()))
    }

    fn write_all(&self, data: &[u8]) -> VfsResult<()> {
        (self.0)(SimpleFileOperation::Write(data)).map(|_| ())
    }
}

pub trait SimpleFileContent {
    /// Converts the content into bytes.
    fn into_content(self) -> Cow<'static, [u8]>;
}

impl SimpleFileContent for Vec<u8> {
    fn into_content(self) -> Cow<'static, [u8]> {
        Cow::Owned(self)
    }
}

impl SimpleFileContent for String {
    fn into_content(self) -> Cow<'static, [u8]> {
        Cow::Owned(self.into_bytes())
    }
}

impl SimpleFileContent for &'static str {
    fn into_content(self) -> Cow<'static, [u8]> {
        Cow::Borrowed(self.as_bytes())
    }
}

impl SimpleFileContent for &'static [u8] {
    fn into_content(self) -> Cow<'static, [u8]> {
        Cow::Borrowed(self)
    }
}

impl<F, R> SimpleFileOps for F
where
    F: Fn() -> VfsResult<R> + Send + Sync + 'static,
    R: SimpleFileContent,
{
    fn read_all(&self) -> VfsResult<Cow<'_, [u8]>> {
        Ok((self)()?.into_content())
    }
}

/// A simple file.
pub struct SimpleFile {
    node: SimpleFsNode,
    ops: Arc<dyn SimpleFileOps>,
}

impl SimpleFile {
    /// Creates a simple file from given file operations.
    pub fn new(fs: Arc<SimpleFs>, ty: NodeType, ops: impl SimpleFileOps) -> Arc<Self> {
        let node = SimpleFsNode::new(fs, ty, NodePermission::default());
        Arc::new(Self {
            node,
            ops: Arc::new(ops),
        })
    }

    /// Creates a simple file from given file operations.
    pub fn new_regular(fs: Arc<SimpleFs>, ops: impl SimpleFileOps) -> Arc<Self> {
        Self::new(fs, NodeType::RegularFile, ops)
    }
}

#[inherit_methods(from = "self.node")]
impl NodeOps for SimpleFile {
    fn inode(&self) -> u64;

    fn metadata(&self) -> VfsResult<Metadata>;

    fn update_metadata(&self, update: MetadataUpdate) -> VfsResult<()>;

    fn filesystem(&self) -> &dyn FilesystemOps;

    fn sync(&self, data_only: bool) -> VfsResult<()>;

    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }

    fn len(&self) -> VfsResult<u64> {
        Ok(self.ops.read_all()?.len() as u64)
    }

    fn flags(&self) -> NodeFlags {
        NodeFlags::NON_CACHEABLE
    }
}

impl FileNodeOps for SimpleFile {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> VfsResult<usize> {
        let data = self.ops.read_all()?;
        if offset >= data.len() as u64 {
            return Ok(0);
        }
        let data = &data[offset as usize..];
        let read = data.len().min(buf.len());
        buf[..read].copy_from_slice(&data[..read]);
        Ok(read)
    }

    fn write_at(&self, buf: &[u8], offset: u64) -> VfsResult<usize> {
        let data = self.ops.read_all()?;
        if offset == 0 && buf.len() >= data.len() {
            self.ops.write_all(buf)?;
            return Ok(buf.len());
        }
        let mut data = data.to_vec();
        let end_pos = offset + buf.len() as u64;
        if end_pos > data.len() as u64 {
            data.resize(end_pos as usize, 0);
        }
        data[offset as usize..end_pos as usize].copy_from_slice(buf);
        self.ops.write_all(&data)?;
        Ok(buf.len())
    }

    fn append(&self, buf: &[u8]) -> VfsResult<(usize, u64)> {
        let mut data = self.ops.read_all()?.to_vec();
        data.extend_from_slice(buf);
        self.ops.write_all(&data)?;
        Ok((buf.len(), data.len() as u64))
    }

    fn set_len(&self, len: u64) -> VfsResult<()> {
        let data = self.ops.read_all()?;
        match len.cmp(&(data.len() as u64)) {
            Ordering::Less => self.ops.write_all(&data[..len as usize]),
            Ordering::Greater => {
                let mut data = data.to_vec();
                data.resize(len as usize, 0);
                self.ops.write_all(&data)
            }
            _ => Ok(()),
        }
    }

    fn set_symlink(&self, target: &str) -> VfsResult<()> {
        self.ops.write_all(target.as_bytes())
    }
}

impl FsPollable for SimpleFile {
    fn poll(&self) -> FsIoEvents {
        FsIoEvents::IN | FsIoEvents::OUT
    }

    fn register(&self, _context: &mut Context<'_>, _events: FsIoEvents) {}
}

impl Pollable for SimpleFile {
    fn poll(&self) -> IoEvents {
        fs_events_to_io(FsPollable::poll(self))
    }

    fn register(&self, context: &mut Context<'_>, events: IoEvents) {
        FsPollable::register(self, context, io_events_to_fs(events));
    }
}

/// A special file that directly implements file operations without caching content in the kernel.
/// It is used for files in procfs and debugfs that need to reflect real-time data.
pub struct SpecialFsFile<T: DirectRwFsFileOps> {
    node: SimpleFsNode,
    ops: Arc<T>,
}

pub trait DirectRwFsFileOps: Send + Sync + 'static {
    /// Reads a number of bytes starting from a given offset.
    fn read_at(&self, buf: &mut [u8], offset: u64) -> VfsResult<usize>;
    /// Writes a number of bytes starting from a given offset.
    fn write_at(&self, _buf: &[u8], _offset: u64) -> VfsResult<usize> {
        Err(VfsError::InvalidInput)
    }
}

impl<T: DirectRwFsFileOps> SpecialFsFile<T> {
    /// Creates a file from given file object and specified permissions.
    pub fn new_with_perm(
        fs: Arc<SimpleFs>,
        ty: NodeType,
        obj: T,
        perm: NodePermission,
    ) -> Arc<Self> {
        let node = SimpleFsNode::new(fs, ty, perm);
        Arc::new(Self {
            node,
            ops: Arc::new(obj),
        })
    }

    /// Creates a regular file from given file operations object and specified permissions.
    pub fn new_regular_with_perm(fs: Arc<SimpleFs>, obj: T, perm: NodePermission) -> Arc<Self> {
        Self::new_with_perm(fs, NodeType::RegularFile, obj, perm)
    }
}

#[inherit_methods(from = "self.node")]
impl<T: DirectRwFsFileOps> NodeOps for SpecialFsFile<T> {
    fn inode(&self) -> u64;

    fn metadata(&self) -> VfsResult<Metadata>;

    fn update_metadata(&self, update: MetadataUpdate) -> VfsResult<()>;

    fn filesystem(&self) -> &dyn FilesystemOps;

    fn sync(&self, data_only: bool) -> VfsResult<()>;

    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }

    fn len(&self) -> VfsResult<u64> {
        Ok(0)
    }

    fn flags(&self) -> NodeFlags {
        NodeFlags::NON_CACHEABLE
    }
}

impl<T: DirectRwFsFileOps> FsPollable for SpecialFsFile<T> {
    fn poll(&self) -> FsIoEvents {
        // TODO: support poll for special files when needed
        FsIoEvents::IN | FsIoEvents::OUT
    }

    fn register(&self, _context: &mut Context<'_>, _events: FsIoEvents) {
        // SpecialFsFile reports itself as always-ready via `poll()` (IN|OUT),
        // so registration is a no-op. Matches `SimpleFile::register` above —
        // turning this into `unimplemented!()` was a regression that panicked
        // the kernel on any `epoll_ctl` against debugfs/procfs special files
        // (tracepoint trace_pipe, saved_cmdlines, dyn_debug controls, …).
    }
}

impl<T: DirectRwFsFileOps> Pollable for SpecialFsFile<T> {
    fn poll(&self) -> IoEvents {
        fs_events_to_io(FsPollable::poll(self))
    }

    fn register(&self, context: &mut Context<'_>, events: IoEvents) {
        FsPollable::register(self, context, io_events_to_fs(events));
    }
}

impl<T: DirectRwFsFileOps> FileNodeOps for SpecialFsFile<T> {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> VfsResult<usize> {
        self.ops.read_at(buf, offset)
    }

    fn write_at(&self, buf: &[u8], offset: u64) -> VfsResult<usize> {
        self.ops.write_at(buf, offset)
    }

    fn append(&self, buf: &[u8]) -> VfsResult<(usize, u64)> {
        let w = self.ops.write_at(buf, 0)?;
        Ok((w, 0))
    }

    fn set_len(&self, len: u64) -> VfsResult<()> {
        if len == 0 {
            // Shell redirection usually opens these files with O_TRUNC.
            return Ok(());
        }
        Err(VfsError::InvalidInput)
    }

    fn set_symlink(&self, _target: &str) -> VfsResult<()> {
        Err(VfsError::InvalidInput)
    }
}

// TODO: create a linux like seq file that supports iterating content in chunks instead of reading all content at once, to avoid large memory usage for large files.
/// A Sequential file, which only supports reading all content. It is used for procfs and sysfs.
pub struct SeqObject {
    ops: Arc<dyn SimpleFileOps>,
    content_cache: PiMutex<Option<Vec<u8>>>,
}

impl DirectRwFsFileOps for SeqObject {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> VfsResult<usize> {
        let mut cache = self.content_cache.lock();
        if cache.is_none() || offset == 0 {
            let content = self.ops.read_all()?;
            *cache = Some(content.into_owned());
        }

        let data = cache.as_ref().unwrap();
        if offset >= data.len() as u64 {
            return Ok(0);
        }
        let data = &data[offset as usize..];
        let read = data.len().min(buf.len());
        buf[..read].copy_from_slice(&data[..read]);
        Ok(read)
    }
}

impl SeqObject {
    /// Creates a new `SeqObject` instance with given file operations.
    /// Now, we just reuse `SimpleFileOps` for simplicity, but we will likely
    /// need a separate trait for `SeqObject` in the future when we want to support
    /// more features like iterating content.
    pub fn new(ops: impl SimpleFileOps) -> Self {
        Self {
            content_cache: PiMutex::new(None),
            ops: Arc::new(ops),
        }
    }
}
