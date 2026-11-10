//! Paths for kernel objects without a pathname in a task's mount tree.

use alloc::{string::String, sync::Arc};
use core::any::Any;

use ax_lazyinit::LazyLock;
use axfs_ng_vfs::{
    DirEntry, FileNode, FileNodeOps, FilesystemOps, Location, Metadata, MetadataUpdate, Mountpoint,
    NodeFlags, NodeIdentity, NodeOps, NodePermission, NodeType, Reference, VfsError, VfsResult,
    WeakDirEntry,
};
use axpoll::{IoEvents, Pollable};

use super::{DirMapping, SimpleDir, SimpleFs, SimpleFsNode};
use crate::{sync::Mutex, task::try_current_user_task};

// Pseudo mounts are permanent, but do not retain any object entries.
fn object_fs(name: &str, magic: u32) -> (Arc<SimpleFs>, Arc<Mountpoint>) {
    let mut owner = None;
    let filesystem = SimpleFs::new_with(name.into(), magic, |fs| {
        owner = Some(fs.clone());
        SimpleDir::new_maker(fs, Arc::new(DirMapping::new()))
    });
    (
        owner.expect("object filesystem owner"),
        Mountpoint::new_root(&filesystem),
    )
}

static PIPE_FS: LazyLock<(Arc<SimpleFs>, Arc<Mountpoint>)> =
    LazyLock::new(|| object_fs("pipefs", 0x50495045));
static SOCKET_FS: LazyLock<(Arc<SimpleFs>, Arc<Mountpoint>)> =
    LazyLock::new(|| object_fs("sockfs", 0x534f434b));
static NAMESPACE_FS: LazyLock<(Arc<SimpleFs>, Arc<Mountpoint>)> =
    LazyLock::new(|| object_fs("nsfs", 0x6e736673));

/// Inode metadata shared by the real object and every path referring to it.
pub(crate) struct ObjectInode {
    node: SimpleFsNode,
    mount: Arc<Mountpoint>,
    identity: NodeIdentity,
    entry: Mutex<Option<WeakDirEntry>>,
}

impl ObjectInode {
    pub(crate) fn new(ty: NodeType, mode: u16) -> Arc<Self> {
        let fs = match ty {
            NodeType::Fifo => &*PIPE_FS,
            NodeType::Socket => &*SOCKET_FS,
            _ => &*NAMESPACE_FS,
        };
        let node = SimpleFsNode::new(fs.0.clone(), ty, NodePermission::from_bits_truncate(mode));
        {
            let mut metadata = node.metadata.lock_irqsave();
            metadata.device = fs.1.device();
            metadata.block_size = 4096;
        }
        // Kernel-created objects have no userspace credential owner.
        if let Some(task) = try_current_user_task().expect("valid Starry task extension") {
            let cred = task.as_thread().cred();
            let mut metadata = node.metadata.lock_irqsave();
            metadata.uid = cred.fsuid;
            metadata.gid = cred.fsgid;
        }
        let identity = NodeIdentity::new(alloc::vec![Arc::as_ptr(&fs.0) as u64, 0, node.inode()]);
        Arc::new(Self {
            node,
            mount: fs.1.clone(),
            identity,
            entry: Mutex::new(None),
        })
    }

    pub(crate) fn metadata(&self) -> VfsResult<Metadata> {
        self.node.metadata()
    }

    pub(crate) fn inode(&self) -> u64 {
        self.node.metadata.lock_irqsave().inode
    }

    /// Pins the original namespace inode number, rather than a materialization ID.
    pub(crate) fn namespace(kind: u64, number: u64) -> Arc<Self> {
        let mut inode = Self::new(NodeType::RegularFile, 0o444);
        let unique = Arc::get_mut(&mut inode).expect("unpublished namespace inode");
        unique.identity = NodeIdentity::new(alloc::vec![
            Arc::as_ptr(&NAMESPACE_FS.0) as u64,
            1,
            kind,
            number,
        ]);
        {
            let mut metadata = unique.node.metadata.lock_irqsave();
            metadata.inode = number;
            metadata.uid = 0;
            metadata.gid = 0;
            metadata.block_size = 4096;
        }
        inode
    }

    /// Canonicalizes live paths without retaining an object-to-entry cycle.
    pub(crate) fn location<T: Send + Sync + 'static>(self: &Arc<Self>, object: T) -> Location {
        let existing = self.entry.lock().as_ref().and_then(WeakDirEntry::upgrade);
        if let Some(entry) = existing {
            return Location::new(self.mount.clone(), entry);
        }
        let candidate = DirEntry::new_file(
            FileNode::new(Arc::new(ObjectNode {
                inode: self.clone(),
                object,
            })),
            self.node
                .metadata()
                .expect("in-memory object metadata")
                .node_type,
            Reference::new(None, String::new()),
        );
        let entry = {
            let mut cached = self.entry.lock();
            if let Some(entry) = cached.as_ref().and_then(WeakDirEntry::upgrade) {
                entry
            } else {
                *cached = Some(candidate.downgrade());
                candidate.clone()
            }
        };
        // Drop a losing candidate only after releasing the cache lock.
        Location::new(self.mount.clone(), entry)
    }
}

/// A native object reference, never a high-level file description.
pub(crate) struct ObjectNode<T> {
    pub(crate) inode: Arc<ObjectInode>,
    pub(crate) object: T,
}

impl<T: Send + Sync + 'static> NodeOps for ObjectNode<T> {
    fn inode(&self) -> u64 {
        self.inode.metadata().expect("in-memory metadata").inode
    }
    fn metadata(&self) -> VfsResult<Metadata> {
        self.inode.metadata()
    }
    fn update_metadata(&self, update: MetadataUpdate) -> VfsResult<()> {
        self.inode.node.update_metadata(update)
    }
    fn filesystem(&self) -> &dyn FilesystemOps {
        self.inode.node.filesystem()
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
    fn identity(&self) -> Option<NodeIdentity> {
        Some(self.inode.identity.clone())
    }
}

impl<T: Send + Sync + 'static> FileNodeOps for ObjectNode<T> {
    fn read_at(&self, _buf: &mut [u8], _offset: u64) -> VfsResult<usize> {
        Err(VfsError::InvalidInput)
    }
    fn write_at(&self, _buf: &[u8], _offset: u64) -> VfsResult<usize> {
        Err(VfsError::InvalidInput)
    }
    fn append(&self, _buf: &[u8]) -> VfsResult<(usize, u64)> {
        Err(VfsError::InvalidInput)
    }
    fn set_len(&self, _len: u64) -> VfsResult<()> {
        Err(VfsError::InvalidInput)
    }
}

impl<T: Send + Sync + 'static> Pollable for ObjectNode<T> {
    fn poll(&self) -> IoEvents {
        IoEvents::empty()
    }
    /// # Safety
    /// This follows the task/deferred-context registration contract. Object
    /// vnodes have no I/O events; opened descriptions supply their own sources.
    unsafe fn register_shared(
        &self,
        _sink: &mut dyn axpoll::SharedRegistrationSink,
        _events: IoEvents,
    ) {
    }
}
