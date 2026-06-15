use alloc::{sync::Arc, vec::Vec};
use core::{any::Any, task::Context};

use ax_fs_ng::vfs::CachedFile;
use ax_memory_addr::{PhysAddr, PhysAddrRange};
use axfs_ng_vfs::{
    DeviceId, FileNodeOps, FilesystemOps, FsIoEvents, FsPollable, Metadata, MetadataUpdate,
    NodeFlags, NodeOps, NodePermission, NodeType, VfsError, VfsResult,
};
use axpoll::{IoEvents, Pollable};
use inherit_methods_macro::inherit_methods;

use super::{SimpleFs, SimpleFsNode};

fn fs_events_to_io(events: FsIoEvents) -> IoEvents {
    IoEvents::from_bits_truncate(events.bits())
}

fn io_events_to_fs(events: IoEvents) -> FsIoEvents {
    FsIoEvents::from_bits_truncate(events.bits())
}

/// Mmap behavior for devices.
#[derive(Clone)]
pub enum DeviceMmap {
    /// The device is not mappable (→ ENODEV, matches Linux).
    None,
    /// Maps to a physical address range. The optional retainer keeps
    /// driver-owned backing pages alive for as long as any VMA built
    /// from this mapping exists — pinned by the resulting
    /// [`LinearBackend`] so userspace can't observe freed memory if
    /// the device drops the buffer before munmap.
    Physical(PhysAddrRange, Option<Arc<dyn Any + Send + Sync>>),
    /// Maps to an already offset-resolved physical address range.
    ///
    /// This is for file descriptors whose mmap offset is a selector rather than
    /// a byte offset into a linear device, such as io_uring ring offsets.
    PhysicalResolved(PhysAddrRange, Option<Arc<dyn Any + Send + Sync>>),
    /// Maps to an explicit physical page list for this exact mmap request.
    /// The producer has already applied the requested offset and length, so
    /// mmap callers must map these pages in order without adding the offset
    /// again. This covers layouts that are not a single contiguous physical
    /// range, such as BPF ringbuf maps that expose mirrored data pages.
    PhysicalPages(Vec<PhysAddr>, Option<Arc<dyn Any + Send + Sync>>),
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

    /// Returns the memory mapping behavior of the device for the given offset.
    ///
    /// # Arguments
    /// * `offset` - The offset from the start of the device
    /// * `length` - The length of the mapping
    fn mmap(&self, _offset: u64, _length: u64) -> DeviceMmap {
        DeviceMmap::None
    }

    /// Returns the flags for the device node.
    fn flags(&self) -> NodeFlags {
        NodeFlags::empty()
    }

    /// Called when the device is opened. `exclusive` is true if O_EXCL was set.
    fn open(&self, _exclusive: bool) -> VfsResult<()> {
        Ok(())
    }

    /// Called when the last file descriptor to this device is closed.
    fn close(&self, _exclusive: bool) {}
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

    /// Returns the memory mapping behavior of the device for the given offset.
    pub fn mmap(&self, offset: u64, length: u64) -> DeviceMmap {
        self.ops.mmap(offset, length)
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

impl FsPollable for Device {
    fn poll(&self) -> FsIoEvents {
        if let Some(pollable) = self.ops.as_pollable() {
            io_events_to_fs(pollable.poll())
        } else {
            FsIoEvents::IN | FsIoEvents::OUT
        }
    }

    fn register(&self, context: &mut Context<'_>, events: FsIoEvents) {
        if let Some(pollable) = self.ops.as_pollable() {
            pollable.register(context, fs_events_to_io(events));
        }
    }
}

impl Pollable for Device {
    fn poll(&self) -> IoEvents {
        fs_events_to_io(FsPollable::poll(self))
    }

    fn register(&self, context: &mut Context<'_>, events: IoEvents) {
        FsPollable::register(self, context, io_events_to_fs(events));
    }
}
