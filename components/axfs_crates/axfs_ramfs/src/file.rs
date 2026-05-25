use alloc::{string::String, sync::Arc, vec::Vec};
use core::{any::Any, cmp::min, ops::Deref, task::Context, time::Duration};

use ax_fs_vfs::{
    DeviceId, FileNode as VfsFileNode, FileNodeOps, FilesystemOps, Metadata, MetadataUpdate,
    NodeFlags, NodeOps, NodePermission, NodeType, VfsError, VfsResult,
};
use ax_kspin::SpinNoIrq as Mutex;
use axpoll::{IoEvents, Pollable};

use crate::RamFileSystem;

pub struct FileNode {
    fs: Arc<RamFileSystem>,
    inode: u64,
    mode: Mutex<NodePermission>,
    inner: Mutex<FileInner>,
}

#[derive(Default)]
struct FileInner {
    content: Vec<u8>,
    symlink: Option<String>,
}

impl FileNode {
    pub(crate) fn make(
        fs: Arc<RamFileSystem>,
        inode: u64,
        mode: NodePermission,
        symlink: Option<String>,
    ) -> VfsFileNode {
        VfsFileNode::new(Arc::new(Self {
            fs,
            inode,
            mode: Mutex::new(mode),
            inner: Mutex::new(FileInner {
                content: Vec::new(),
                symlink,
            }),
        }))
    }

    fn node_type(&self) -> NodeType {
        if self.inner.lock().symlink.is_some() {
            NodeType::Symlink
        } else {
            NodeType::RegularFile
        }
    }
}

impl NodeOps for FileNode {
    fn inode(&self) -> u64 {
        self.inode
    }

    fn metadata(&self) -> VfsResult<Metadata> {
        let inner = self.inner.lock();
        Ok(Metadata {
            device: 0,
            inode: self.inode,
            nlink: 1,
            mode: *self.mode.lock(),
            node_type: if inner.symlink.is_some() {
                NodeType::Symlink
            } else {
                NodeType::RegularFile
            },
            uid: 0,
            gid: 0,
            size: inner
                .symlink
                .as_ref()
                .map_or(inner.content.len(), String::len) as u64,
            block_size: 4096,
            blocks: inner.content.len().div_ceil(512) as u64,
            rdev: DeviceId::default(),
            atime: Duration::default(),
            mtime: Duration::default(),
            ctime: Duration::default(),
        })
    }

    fn update_metadata(&self, update: MetadataUpdate) -> VfsResult<()> {
        if let Some(mode) = update.mode {
            *self.mode.lock() = mode;
        }
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
        NodeFlags::ALWAYS_CACHE
    }
}

impl FileNodeOps for FileNode {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> VfsResult<usize> {
        let inner = self.inner.lock();
        let data = inner
            .symlink
            .as_ref()
            .map_or(inner.content.as_slice(), String::as_bytes);
        let start = min(offset as usize, data.len());
        let end = min(start + buf.len(), data.len());
        let src = &data[start..end];
        buf[..src.len()].copy_from_slice(src);
        Ok(src.len())
    }

    fn write_at(&self, buf: &[u8], offset: u64) -> VfsResult<usize> {
        if self.node_type() == NodeType::Symlink {
            return Err(VfsError::InvalidInput);
        }
        let offset = offset as usize;
        let mut inner = self.inner.lock();
        if offset + buf.len() > inner.content.len() {
            inner.content.resize(offset + buf.len(), 0);
        }
        inner.content[offset..offset + buf.len()].copy_from_slice(buf);
        Ok(buf.len())
    }

    fn append(&self, buf: &[u8]) -> VfsResult<(usize, u64)> {
        if self.node_type() == NodeType::Symlink {
            return Err(VfsError::InvalidInput);
        }
        let mut inner = self.inner.lock();
        inner.content.extend_from_slice(buf);
        Ok((buf.len(), inner.content.len() as u64))
    }

    fn set_len(&self, len: u64) -> VfsResult<()> {
        if self.node_type() == NodeType::Symlink {
            return Err(VfsError::InvalidInput);
        }
        self.inner.lock().content.resize(len as usize, 0);
        Ok(())
    }

    fn set_symlink(&self, target: &str) -> VfsResult<()> {
        let mut inner = self.inner.lock();
        inner.content.clear();
        inner.symlink = Some(target.into());
        Ok(())
    }
}

impl Pollable for FileNode {
    fn poll(&self) -> IoEvents {
        IoEvents::IN | IoEvents::OUT
    }

    fn register(&self, _context: &mut Context<'_>, _events: IoEvents) {}
}
