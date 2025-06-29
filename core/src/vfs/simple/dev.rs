use core::any::Any;

use alloc::sync::Arc;
use axfs_ng_vfs::{
    DeviceId, FileNodeOps, FilesystemOps, Metadata, MetadataUpdate, NodeOps, NodePermission,
    NodeType, VfsError, VfsResult,
};
use inherit_methods_macro::inherit_methods;
use lock_api::RawMutex;

use crate::vfs::simple::{SimpleFs, SimpleFsNode};

/// Trait for device operations.
pub trait DeviceOps: Send + Sync {
    /// Reads data from the device at the specified offset.
    fn read_at(&self, buf: &mut [u8], offset: u64) -> VfsResult<usize>;
    /// Writes data to the device at the specified offset.
    fn write_at(&self, buf: &[u8], offset: u64) -> VfsResult<usize>;

    /// Casts the device operations to a dynamic type.
    fn as_any(&self) -> &dyn Any;
}
impl<F> DeviceOps for F
where
    F: Fn(&mut [u8], u64) -> VfsResult<usize> + Send + Sync + 'static,
{
    fn read_at(&self, buf: &mut [u8], offset: u64) -> VfsResult<usize> {
        (self)(buf, offset)
    }

    fn write_at(&self, _buf: &[u8], _offset: u64) -> VfsResult<usize> {
        Err(VfsError::EBADF)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// A device node in the filesystem.
pub struct Device<M: RawMutex> {
    node: SimpleFsNode<M>,
    ops: Arc<dyn DeviceOps>,
}
impl<M: RawMutex + Send + Sync + 'static> Device<M> {
    pub(crate) fn new(
        fs: Arc<SimpleFs<M>>,
        node_type: NodeType,
        device_id: DeviceId,
        ops: impl DeviceOps + 'static,
    ) -> Arc<Self> {
        let node = SimpleFsNode::new(fs, node_type, NodePermission::default());
        node.metadata.lock().rdev = device_id;
        Arc::new(Self {
            node,
            ops: Arc::new(ops),
        })
    }

    /// Returns the inner device operations.
    pub fn inner(&self) -> &Arc<dyn DeviceOps> {
        &self.ops
    }
}

#[inherit_methods(from = "self.node")]
impl<M: RawMutex + Send + Sync + 'static> NodeOps<M> for Device<M> {
    fn inode(&self) -> u64;
    fn metadata(&self) -> VfsResult<Metadata>;
    fn update_metadata(&self, update: MetadataUpdate) -> VfsResult<()>;
    fn filesystem(&self) -> &dyn FilesystemOps<M>;
    fn sync(&self, data_only: bool) -> VfsResult<()>;
    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }

    fn len(&self) -> VfsResult<u64> {
        Ok(0)
    }
}

impl<M: RawMutex + Send + Sync + 'static> FileNodeOps<M> for Device<M> {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> VfsResult<usize> {
        self.ops.read_at(buf, offset)
    }

    fn write_at(&self, buf: &[u8], offset: u64) -> VfsResult<usize> {
        self.ops.write_at(buf, offset)
    }

    fn append(&self, _buf: &[u8]) -> VfsResult<(usize, u64)> {
        Err(VfsError::ENOTTY)
    }

    fn set_len(&self, _len: u64) -> VfsResult<()> {
        // If can write...
        if self.write_at(b"", 0).is_ok() {
            Ok(())
        } else {
            Err(VfsError::EBADF)
        }
    }

    fn set_symlink(&self, _target: &str) -> VfsResult<()> {
        Err(VfsError::EBADF)
    }
}
