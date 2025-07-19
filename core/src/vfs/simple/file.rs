use alloc::{borrow::Cow, sync::Arc, vec::Vec};
use core::{any::Any, cmp::Ordering};

use axfs_ng_vfs::{
    FileNodeOps, FilesystemOps, Metadata, MetadataUpdate, NodeOps, NodePermission, NodeType,
    VfsError, VfsResult,
};
use inherit_methods_macro::inherit_methods;
use lock_api::RawMutex;

use crate::vfs::simple::fs::{SimpleFs, SimpleFsNode};

pub trait SimpleFileOps: Send + Sync {
    fn read_all(&self) -> VfsResult<Cow<[u8]>>;
    fn write_all(&self, data: &[u8]) -> VfsResult<()>;
}

pub enum SimpleFileOperation<'a> {
    Read,
    Write(&'a [u8]),
}

pub struct RwFile<F>(F);

impl<F, R> RwFile<F>
where
    F: Fn(SimpleFileOperation) -> VfsResult<Option<R>> + Send + Sync,
    R: Into<Vec<u8>>,
{
    pub fn new(imp: F) -> Self {
        Self(imp)
    }
}

impl<F, R> SimpleFileOps for RwFile<F>
where
    F: Fn(SimpleFileOperation) -> VfsResult<Option<R>> + Send + Sync,
    R: Into<Vec<u8>>,
{
    fn read_all(&self) -> VfsResult<Cow<[u8]>> {
        (self.0)(SimpleFileOperation::Read).map(|it| Cow::Owned(it.unwrap().into()))
    }

    fn write_all(&self, data: &[u8]) -> VfsResult<()> {
        (self.0)(SimpleFileOperation::Write(data)).map(|_| ())
    }
}

impl<F, R> SimpleFileOps for F
where
    F: Fn() -> VfsResult<R> + Send + Sync + 'static,
    R: Into<Vec<u8>>,
{
    fn read_all(&self) -> VfsResult<Cow<[u8]>> {
        (self)().map(|it| Cow::Owned(it.into()))
    }

    fn write_all(&self, _data: &[u8]) -> VfsResult<()> {
        Err(VfsError::EBADF)
    }
}

pub struct SimpleFile<M: RawMutex> {
    node: SimpleFsNode<M>,
    ops: Arc<dyn SimpleFileOps>,
}

impl<M: RawMutex + Send + Sync + 'static> SimpleFile<M> {
    pub fn new(fs: Arc<SimpleFs<M>>, ops: impl SimpleFileOps + 'static) -> Arc<Self> {
        let node = SimpleFsNode::new(fs, NodeType::RegularFile, NodePermission::default());
        Arc::new(Self {
            node,
            ops: Arc::new(ops),
        })
    }
}

#[inherit_methods(from = "self.node")]
impl<M: RawMutex + Send + Sync + 'static> NodeOps<M> for SimpleFile<M> {
    fn inode(&self) -> u64;

    fn metadata(&self) -> VfsResult<Metadata>;

    fn update_metadata(&self, update: MetadataUpdate) -> VfsResult<()>;

    fn filesystem(&self) -> &dyn FilesystemOps<M>;

    fn sync(&self, data_only: bool) -> VfsResult<()>;

    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }

    fn len(&self) -> VfsResult<u64> {
        Ok(self.ops.read_all()?.len() as u64)
    }
}

impl<M: RawMutex + Send + Sync + 'static> FileNodeOps<M> for SimpleFile<M> {
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
