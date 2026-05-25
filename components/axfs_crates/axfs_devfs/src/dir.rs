use alloc::{collections::BTreeMap, string::String, sync::Arc};
use core::{any::Any, ops::Deref, task::Context, time::Duration};

use ax_fs_vfs::{
    DeviceId, DirEntry, DirEntrySink, DirNode as VfsDirNode, DirNodeOps, FileNode, FilesystemOps,
    Metadata, MetadataUpdate, NodeFlags, NodeOps, NodePermission, NodeType, Reference, VfsError,
    VfsResult, WeakDirEntry,
};
use ax_kspin::SpinNoIrq as Mutex;
use axpoll::{IoEvents, Pollable};

use crate::{
    DeviceFileSystem,
    device::{NullDev, UrandomDev, ZeroDev},
};

pub(crate) struct DevDirNode {
    fs: Arc<DeviceFileSystem>,
    this: WeakDirEntry,
    inode: u64,
    children: Mutex<BTreeMap<String, DirEntry>>,
}

impl DevDirNode {
    pub(crate) fn make(fs: Arc<DeviceFileSystem>, this: WeakDirEntry, inode: u64) -> VfsDirNode {
        VfsDirNode::new(Arc::new(Self {
            fs,
            this,
            inode,
            children: Mutex::new(BTreeMap::new()),
        }))
    }

    pub(crate) fn populate_static_devices(&self) {
        self.add_static(
            "null",
            NullDev::make(self.fs.clone(), self.fs.alloc_inode()),
        );
        self.add_static(
            "zero",
            ZeroDev::make(self.fs.clone(), self.fs.alloc_inode()),
        );
        self.add_static(
            "urandom",
            UrandomDev::make(self.fs.clone(), self.fs.alloc_inode()),
        );
    }

    fn add_static(&self, name: &str, file: FileNode) {
        let entry = DirEntry::new_file(
            file,
            NodeType::CharacterDevice,
            Reference::new(self.this.upgrade(), name.into()),
        );
        self.children.lock().insert(name.into(), entry);
    }
}

impl NodeOps for DevDirNode {
    fn inode(&self) -> u64 {
        self.inode
    }

    fn metadata(&self) -> VfsResult<Metadata> {
        Ok(Metadata {
            device: 0,
            inode: self.inode,
            nlink: 1,
            mode: NodePermission::default(),
            node_type: NodeType::Directory,
            uid: 0,
            gid: 0,
            size: 4096,
            block_size: 4096,
            blocks: 8,
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
        NodeFlags::NON_CACHEABLE
    }
}

impl DirNodeOps for DevDirNode {
    fn read_dir(&self, offset: u64, sink: &mut dyn DirEntrySink) -> VfsResult<usize> {
        let mut count = 0;
        for (index, (name, entry)) in self
            .children
            .lock()
            .iter()
            .enumerate()
            .skip(offset as usize)
        {
            if !sink.accept(name, entry.inode(), entry.node_type(), (index + 1) as u64) {
                break;
            }
            count += 1;
        }
        Ok(count)
    }

    fn lookup(&self, name: &str) -> VfsResult<DirEntry> {
        self.children
            .lock()
            .get(name)
            .cloned()
            .ok_or(VfsError::NotFound)
    }

    fn create(
        &self,
        _name: &str,
        _node_type: NodeType,
        _permission: NodePermission,
    ) -> VfsResult<DirEntry> {
        Err(VfsError::PermissionDenied)
    }

    fn link(&self, _name: &str, _node: &DirEntry) -> VfsResult<DirEntry> {
        Err(VfsError::PermissionDenied)
    }

    fn unlink(&self, _name: &str) -> VfsResult<()> {
        Err(VfsError::PermissionDenied)
    }

    fn rename(&self, _src_name: &str, _dst_dir: &VfsDirNode, _dst_name: &str) -> VfsResult<()> {
        Err(VfsError::PermissionDenied)
    }
}

impl Pollable for DevDirNode {
    fn poll(&self) -> IoEvents {
        IoEvents::IN | IoEvents::OUT
    }

    fn register(&self, _context: &mut Context<'_>, _events: IoEvents) {}
}
