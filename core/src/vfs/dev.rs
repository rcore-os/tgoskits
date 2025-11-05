use alloc::sync::Arc;
use core::{any::Any, task::Context};

use axfs_ng::CachedFile;
use axfs_ng_vfs::{
    DeviceId, FileNodeOps, FilesystemOps, Metadata, MetadataUpdate, NodeFlags, NodeOps,
    NodePermission, NodeType, VfsError, VfsResult,
};
use axpoll::{IoEvents, Pollable};
use inherit_methods_macro::inherit_methods;
use memory_addr::PhysAddrRange;

use super::{SimpleFs, SimpleFsNode};

/// Mmap behavior for devices.
pub enum DeviceMmap {
    /// The device is not mappable.
    None,
    /// Maps to a physical address range.
    Physical(PhysAddrRange),
    /// The device is read-only and will be mapped as CoW.
    ReadOnly,
    /// Maps to a cached file.
    Cache(CachedFile),
}

/// Trait for device operations.
pub trait DeviceOps: Send + Sync {
    /// Reads data from the device at the specified offset.
    fn read_at(&self, buf: &mut [u8], offset: u64) -> VfsResult<usize>;
    /// Writes data to the device at the specified offset.
    fn write_at(&self, buf: &[u8], offset: u64) -> VfsResult<usize>;
    /// Manipulates the underlying device parameters of special files.
    fn ioctl(&self, _cmd: u32, _arg: usize) -> VfsResult<usize> {
        Err(VfsError::NotATty)
    }

    /// Casts the device operations to a dynamic type.
    fn as_any(&self) -> &dyn Any;

    /// Casts the device operations to a [`Pollable`].
    fn as_pollable(&self) -> Option<&dyn Pollable> {
        None
    }

    /// Returns the memory mapping behavior of the device.
    fn mmap(&self) -> DeviceMmap {
        DeviceMmap::None
    }

    /// Returns the flags for the device node.
    fn flags(&self) -> NodeFlags {
        NodeFlags::empty()
    }
}

/// A device node in the filesystem.
pub struct Device {
    node: SimpleFsNode,
    ops: Arc<dyn DeviceOps>,
}

impl Device {
    /// Creates a new device.
    pub fn new(
        fs: Arc<SimpleFs>,
        node_type: NodeType,
        device_id: DeviceId,
        ops: Arc<dyn DeviceOps>,
    ) -> Arc<Self> {
        let node = SimpleFsNode::new(fs, node_type, NodePermission::default());
        node.metadata.lock().rdev = device_id;
        Arc::new(Self { node, ops })
    }

    /// Returns the inner device operations.
    pub fn inner(&self) -> &Arc<dyn DeviceOps> {
        &self.ops
    }

    /// Updates the device ID.
    pub fn set_device_id(&self, device_id: DeviceId) {
        self.node.metadata.lock().rdev = device_id;
    }

    /// Returns the memory mapping behavior of the device.
    pub fn mmap(&self) -> DeviceMmap {
        self.ops.mmap()
    }
}

#[inherit_methods(from = "self.node")]
impl NodeOps for Device {
    fn inode(&self) -> u64;

    fn metadata(&self) -> VfsResult<Metadata>;

    fn update_metadata(&self, update: MetadataUpdate) -> VfsResult<()>;

    fn filesystem(&self) -> &dyn FilesystemOps;

    fn sync(&self, _data_only: bool) -> VfsResult<()> {
        Err(VfsError::InvalidInput)
    }

    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }

    fn len(&self) -> VfsResult<u64> {
        Ok(0)
    }

    fn flags(&self) -> NodeFlags {
        self.ops.flags()
    }
}

impl FileNodeOps for Device {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> VfsResult<usize> {
        self.ops.read_at(buf, offset)
    }

    fn write_at(&self, buf: &[u8], offset: u64) -> VfsResult<usize> {
        self.ops.write_at(buf, offset)
    }

    fn append(&self, _buf: &[u8]) -> VfsResult<(usize, u64)> {
        Err(VfsError::NotATty)
    }

    fn set_len(&self, _len: u64) -> VfsResult<()> {
        // If can write...
        if self.write_at(b"", 0).is_ok() {
            Ok(())
        } else {
            Err(VfsError::BadFileDescriptor)
        }
    }

    fn set_symlink(&self, _target: &str) -> VfsResult<()> {
        Err(VfsError::BadFileDescriptor)
    }

    fn ioctl(&self, cmd: u32, arg: usize) -> VfsResult<usize> {
        self.ops.ioctl(cmd, arg)
    }
}

impl Pollable for Device {
    fn poll(&self) -> IoEvents {
        if let Some(pollable) = self.ops.as_pollable() {
            pollable.poll()
        } else {
            IoEvents::IN | IoEvents::OUT
        }
    }

    fn register(&self, context: &mut Context<'_>, events: IoEvents) {
        if let Some(pollable) = self.ops.as_pollable() {
            pollable.register(context, events);
        }
    }
}
