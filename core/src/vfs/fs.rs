use alloc::{string::String, sync::Arc};
use core::{any::Any, time::Duration};

use axfs_ng_vfs::{
    DeviceId, DirEntry, DirNode, Filesystem, FilesystemOps, Metadata, MetadataUpdate, NodeOps,
    NodePermission, NodeType, Reference, StatFs, VfsResult, path::MAX_NAME_LEN,
};
use lock_api::{Mutex, RawMutex};
use slab::Slab;

use super::DirMaker;

pub fn dummy_stat_fs(fs_type: u32) -> StatFs {
    StatFs {
        fs_type,
        block_size: 512,
        blocks: 100,
        blocks_free: 100,
        blocks_available: 100,

        file_count: 0,
        free_file_count: 0,

        name_length: MAX_NAME_LEN as _,
        fragment_size: 0,
        mount_flags: 0,
    }
}

/// A simple filesystem implementation that uses a slab allocator for inodes.
pub struct SimpleFs<M = axsync::RawMutex> {
    name: String,
    fs_type: u32,
    inodes: Mutex<M, Slab<()>>,
    root: Mutex<M, Option<DirEntry<M>>>,
}

impl<M: RawMutex + Send + Sync + 'static> SimpleFs<M> {
    pub fn new_with(
        name: String,
        fs_type: u32,
        root: impl FnOnce(Arc<Self>) -> DirMaker<M>,
    ) -> Filesystem<M> {
        let fs = Arc::new(Self {
            name,
            fs_type,
            inodes: Mutex::default(),
            root: Mutex::default(),
        });
        let root = root(fs.clone());
        fs.set_root(DirEntry::new_dir(
            |this| DirNode::new(root(this)),
            Reference::root(),
        ));
        Filesystem::new(fs)
    }

    fn set_root(&self, root: DirEntry<M>) {
        *self.root.lock() = Some(root);
    }
}

impl<M: RawMutex> SimpleFs<M> {
    fn alloc_inode(&self) -> u64 {
        self.inodes.lock().insert(()) as u64 + 1
    }

    fn release_inode(&self, ino: u64) {
        self.inodes.lock().remove(ino as usize - 1);
    }
}

impl<M: RawMutex + Send + Sync> FilesystemOps<M> for SimpleFs<M> {
    fn name(&self) -> &str {
        &self.name
    }

    fn root_dir(&self) -> DirEntry<M> {
        self.root.lock().clone().unwrap()
    }

    fn is_cacheable(&self) -> bool {
        false
    }

    fn stat(&self) -> VfsResult<StatFs> {
        Ok(dummy_stat_fs(self.fs_type))
    }
}

pub struct SimpleFsNode<M: RawMutex> {
    fs: Arc<SimpleFs<M>>,
    ino: u64,
    pub(crate) metadata: Mutex<M, Metadata>,
}

impl<M: RawMutex + Send + Sync + 'static> SimpleFsNode<M> {
    pub fn new(fs: Arc<SimpleFs<M>>, node_type: NodeType, mode: NodePermission) -> Self {
        let ino = fs.alloc_inode();
        let metadata = Metadata {
            device: 0,
            inode: ino,
            nlink: 1,
            mode,
            node_type,
            uid: 0,
            gid: 0,
            size: 0,
            block_size: 0,
            blocks: 0,
            rdev: DeviceId::default(),
            atime: Duration::default(),
            mtime: Duration::default(),
            ctime: Duration::default(),
        };
        Self {
            fs,
            ino,
            metadata: Mutex::new(metadata),
        }
    }
}

impl<M: RawMutex> Drop for SimpleFsNode<M> {
    fn drop(&mut self) {
        self.fs.release_inode(self.ino);
    }
}

impl<M: RawMutex + Send + Sync + 'static> NodeOps<M> for SimpleFsNode<M> {
    fn inode(&self) -> u64 {
        self.ino
    }

    fn metadata(&self) -> VfsResult<Metadata> {
        let mut metadata = self.metadata.lock().clone();
        metadata.size = self.len()?;
        Ok(metadata)
    }

    fn len(&self) -> VfsResult<u64> {
        Ok(0)
    }

    fn update_metadata(&self, update: MetadataUpdate) -> VfsResult<()> {
        let mut metadata = self.metadata.lock();
        if let Some(mode) = update.mode {
            metadata.mode = mode;
        }
        if let Some((uid, gid)) = update.owner {
            metadata.uid = uid;
            metadata.gid = gid;
        }
        if let Some(atime) = update.atime {
            metadata.atime = atime;
        }
        if let Some(mtime) = update.mtime {
            metadata.mtime = mtime;
        }
        Ok(())
    }

    fn filesystem(&self) -> &dyn FilesystemOps<M> {
        self.fs.as_ref()
    }

    fn sync(&self, _data_only: bool) -> VfsResult<()> {
        Ok(())
    }

    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }
}
