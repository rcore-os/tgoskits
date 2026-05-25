use alloc::sync::Arc;
use core::{
    any::Any,
    ops::Deref,
    sync::atomic::{AtomicU64, Ordering},
    task::Context,
    time::Duration,
};

use ax_fs_vfs::{
    DeviceId, FileNode, FileNodeOps, FilesystemOps, Metadata, MetadataUpdate, NodeFlags, NodeOps,
    NodePermission, NodeType, VfsResult,
};
use axpoll::{IoEvents, Pollable};

use crate::DeviceFileSystem;

struct DevNode {
    fs: Arc<DeviceFileSystem>,
    inode: u64,
    kind: DeviceKind,
}

#[derive(Clone, Copy)]
enum DeviceKind {
    Null,
    Zero,
    Urandom,
}

impl DevNode {
    fn make(fs: Arc<DeviceFileSystem>, inode: u64, kind: DeviceKind) -> FileNode {
        FileNode::new(Arc::new(Self { fs, inode, kind }))
    }
}

impl NodeOps for DevNode {
    fn inode(&self) -> u64 {
        self.inode
    }

    fn metadata(&self) -> VfsResult<Metadata> {
        Ok(Metadata {
            device: 0,
            inode: self.inode,
            nlink: 1,
            mode: NodePermission::default(),
            node_type: NodeType::CharacterDevice,
            uid: 0,
            gid: 0,
            size: 0,
            block_size: 4096,
            blocks: 0,
            rdev: DeviceId::default(),
            atime: Duration::default(),
            mtime: Duration::default(),
            ctime: Duration::default(),
        })
    }

    fn update_metadata(&self, _update: MetadataUpdate) -> VfsResult<()> {
        Ok(())
    }

    fn filesystem(&self) -> &dyn FilesystemOps {
        self.fs.deref()
    }

    fn sync(&self, _data_only: bool) -> VfsResult<()> {
        Ok(())
    }

    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }

    fn flags(&self) -> NodeFlags {
        NodeFlags::STREAM | NodeFlags::NON_CACHEABLE
    }
}

impl FileNodeOps for DevNode {
    fn read_at(&self, buf: &mut [u8], _offset: u64) -> VfsResult<usize> {
        match self.kind {
            DeviceKind::Null => Ok(0),
            DeviceKind::Zero => {
                buf.fill(0);
                Ok(buf.len())
            }
            DeviceKind::Urandom => {
                fill_random(buf);
                Ok(buf.len())
            }
        }
    }

    fn write_at(&self, buf: &[u8], _offset: u64) -> VfsResult<usize> {
        Ok(buf.len())
    }

    fn append(&self, buf: &[u8]) -> VfsResult<(usize, u64)> {
        Ok((buf.len(), 0))
    }

    fn set_len(&self, _len: u64) -> VfsResult<()> {
        Ok(())
    }

    fn set_symlink(&self, _target: &str) -> VfsResult<()> {
        Err(ax_fs_vfs::VfsError::InvalidInput)
    }
}

impl Pollable for DevNode {
    fn poll(&self) -> IoEvents {
        IoEvents::IN | IoEvents::OUT
    }

    fn register(&self, _context: &mut Context<'_>, _events: IoEvents) {}
}

static RANDOM_SEED: AtomicU64 = AtomicU64::new(0xa2ce_a2ce);

fn next_u64() -> u64 {
    let mut current = RANDOM_SEED.load(Ordering::Relaxed);
    loop {
        let next = current.wrapping_mul(6364136223846793005).wrapping_add(1);
        match RANDOM_SEED.compare_exchange_weak(current, next, Ordering::Relaxed, Ordering::Relaxed)
        {
            Ok(_) => return next,
            Err(observed) => current = observed,
        }
    }
}

fn fill_random(buf: &mut [u8]) {
    for chunk in buf.chunks_mut(8) {
        let bytes = next_u64().to_ne_bytes();
        chunk.copy_from_slice(&bytes[..chunk.len()]);
    }
}

pub struct NullDev;

impl NullDev {
    pub(crate) fn make(fs: Arc<DeviceFileSystem>, inode: u64) -> FileNode {
        DevNode::make(fs, inode, DeviceKind::Null)
    }
}

pub struct ZeroDev;

impl ZeroDev {
    pub(crate) fn make(fs: Arc<DeviceFileSystem>, inode: u64) -> FileNode {
        DevNode::make(fs, inode, DeviceKind::Zero)
    }
}

pub struct UrandomDev;

impl UrandomDev {
    pub(crate) fn make(fs: Arc<DeviceFileSystem>, inode: u64) -> FileNode {
        DevNode::make(fs, inode, DeviceKind::Urandom)
    }
}
