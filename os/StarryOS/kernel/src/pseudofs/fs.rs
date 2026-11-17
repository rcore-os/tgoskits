use alloc::{string::String, sync::Arc};
use core::{any::Any, time::Duration};

use ax_memory_addr::PAGE_SIZE_4K;
use axfs_ng_vfs::{
    AuthorizedMetadataChange, DeviceId, DirEntry, DirNode, Filesystem, FilesystemOps,
    LockedMetadataAttempt, Metadata, MetadataRequest, MetadataUpdate, NamespaceCompletion,
    NamespaceOutcome, NodeOps, NodePermission, NodeType, Reference, StatFs, VfsResult,
    path::MAX_NAME_LEN,
};
use slab::Slab;

use super::DirMaker;
use crate::sync::{Mutex, MutexGuard, RawSpinLock};

/// A simple filesystem implementation that uses a slab allocator for inodes.
pub struct SimpleFs {
    name: String,
    fs_type: u32,
    inodes: RawSpinLock<Slab<()>>,
    root: RawSpinLock<Option<DirEntry>>,
    namespace: Mutex<()>,
}

impl SimpleFs {
    /// Creates a new simple filesystem.
    pub fn new_with(
        name: String,
        fs_type: u32,
        root: impl FnOnce(Arc<Self>) -> DirMaker,
    ) -> Filesystem {
        Self::new_with_handle(name, fs_type, root).0
    }

    /// Creates one filesystem and retains its concrete producer for rooted views.
    pub(crate) fn new_with_handle(
        name: String,
        fs_type: u32,
        root: impl FnOnce(Arc<Self>) -> DirMaker,
    ) -> (Filesystem, Arc<Self>) {
        let fs = Arc::new(Self {
            name,
            fs_type,
            inodes: RawSpinLock::new(Slab::new()),
            root: RawSpinLock::new(None),
            namespace: Mutex::new(()),
        });
        let root = root(fs.clone());
        fs.set_root(DirEntry::new_dir(
            |this| DirNode::new(root(this)),
            Reference::root(),
        ));
        (Filesystem::new(fs.clone()), fs)
    }

    fn set_root(&self, root: DirEntry) {
        *self.root.lock_irqsave() = Some(root);
    }

    fn alloc_inode(&self) -> u64 {
        self.inodes.lock_irqsave().insert(()) as u64 + 1
    }

    fn release_inode(&self, ino: u64) {
        self.inodes.lock_irqsave().remove(ino as usize - 1);
    }
}

impl FilesystemOps for SimpleFs {
    fn name(&self) -> &str {
        &self.name
    }

    fn root_dir(&self) -> DirEntry {
        self.root.lock_irqsave().clone().unwrap()
    }

    fn stat(&self) -> VfsResult<StatFs> {
        // Linux simple_statfs reports page geometry without block-capacity accounting.
        Ok(StatFs {
            fs_type: self.fs_type,
            block_size: PAGE_SIZE_4K as u32,
            blocks: 0,
            blocks_free: 0,
            blocks_available: 0,
            file_count: 0,
            free_file_count: 0,
            name_length: MAX_NAME_LEN as u32,
            fragment_size: PAGE_SIZE_4K as u32,
            mount_flags: 0,
        })
    }
}

/// Filesystem node for [`SimpleFs`].
pub struct SimpleFsNode {
    fs: Arc<SimpleFs>,
    ino: u64,
    // RawSpinLock instead of Mutex: metadata may be read/updated on paths that
    // are already in atomic context (IRQs disabled), so a blocking mutex would
    // trigger a might_sleep() panic.
    pub(crate) metadata: RawSpinLock<Metadata>,
}

impl SimpleFsNode {
    pub(super) fn lock_namespace(&self) -> MutexGuard<'_, ()> {
        self.fs.namespace.lock()
    }

    /// Creates a new filesystem node.
    pub fn new(fs: Arc<SimpleFs>, node_type: NodeType, mode: NodePermission) -> Self {
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
            metadata: RawSpinLock::new(metadata),
        }
    }
}

impl Drop for SimpleFsNode {
    fn drop(&mut self) {
        self.fs.release_inode(self.ino);
    }
}

impl NodeOps for SimpleFsNode {
    fn inode(&self) -> u64 {
        self.ino
    }

    fn metadata(&self) -> VfsResult<Metadata> {
        let mut metadata = self.metadata.lock_irqsave().clone();
        // A non-zero stored size is a fixed inode width recorded via
        // `SimpleFile::set_fixed_size` (e.g. mqueuefs `FILENT_SIZE` = 80, which
        // Linux `mqueue_get_inode` stamps regardless of the rendered status
        // line's length); otherwise report the live content length.
        if metadata.size == 0 {
            metadata.size = self.len()?;
        }
        Ok(metadata)
    }

    fn len(&self) -> VfsResult<u64> {
        Ok(0)
    }

    fn update_metadata(&self, update: MetadataUpdate) -> VfsResult<()> {
        let mut metadata = self.metadata.lock_irqsave();
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

    fn change_metadata(&self, request: &MetadataRequest<'_>) -> NamespaceCompletion<Metadata> {
        if let Err(error) = request.admission().check_owner(self.fs.as_ref()) {
            return NamespaceCompletion::new(NamespaceOutcome::unchanged(Err(error)), None);
        }
        let outcome = {
            let mut metadata = self.metadata.lock_irqsave();
            let mut attempt = SimpleMetadataAttempt {
                metadata: &mut metadata,
            };
            request.execute(&mut attempt)
        };
        NamespaceCompletion::new(outcome, None)
    }

    fn filesystem(&self) -> &dyn FilesystemOps {
        self.fs.as_ref()
    }

    fn sync(&self, _data_only: bool) -> VfsResult<()> {
        Ok(())
    }

    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }
}

struct SimpleMetadataAttempt<'a> {
    metadata: &'a mut Metadata,
}

impl LockedMetadataAttempt for SimpleMetadataAttempt<'_> {
    fn observe(&mut self) -> VfsResult<Metadata> {
        Ok(self.metadata.clone())
    }

    fn wall_time(&mut self) -> Duration {
        // The prewarmed platform clock only reads counters and cached atomics.
        ax_runtime::hal::time::wall_time()
    }

    fn commit(&mut self, change: AuthorizedMetadataChange<'_>) -> NamespaceOutcome<Metadata> {
        change.apply(self.metadata);
        NamespaceOutcome::applied(self.metadata.clone(), Ok(()))
    }
}

#[cfg(all(test, axtest))]
mod tests {
    use alloc::sync::Arc;
    use core::sync::atomic::{AtomicBool, Ordering};

    use ax_fs_ng::{FsContext, FsView};
    use axfs_ng_vfs::{
        MetadataUpdate, Mountpoint, NativeLinkCaller, NodePermission, TrustedInitialization,
    };

    use super::SimpleFs;
    use crate::pseudofs::{DirMapping, SimpleDir, SimpleFile};

    struct BackingRetirement(Arc<AtomicBool>);

    impl Drop for BackingRetirement {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }

    #[axtest::axtest]
    fn simple_filesystem_paths_preserve_metadata_and_retire_the_actual_owner() {
        let retired = Arc::new(AtomicBool::new(false));
        let backing_retirement = BackingRetirement(retired.clone());
        let (filesystem, owner) =
            SimpleFs::new_with_handle("simple-owner-test".into(), 0x53494d50, move |fs| {
                let mut children = DirMapping::new();
                children.add(
                    "retained",
                    SimpleFile::new_regular(fs.clone(), move || {
                        let _ = &backing_retirement;
                        Ok("retained object contents")
                    }),
                );
                SimpleDir::new_maker(fs, Arc::new(children))
            });
        let weak_owner = Arc::downgrade(&owner);
        drop(owner);
        let mount = Mountpoint::new_root(&filesystem);
        let context = FsContext::from_tree(mount.clone(), mount.root_location()).unwrap();
        let caller = NativeLinkCaller::root();
        let view = FsView::native(context, &caller);
        let path = view.resolve("/retained").unwrap();
        let inode = path.location().metadata().unwrap().inode;
        TrustedInitialization::kernel()
            .restore_metadata(
                path.location(),
                MetadataUpdate {
                    mode: Some(NodePermission::from_bits_truncate(0o620)),
                    owner: Some((101, 202)),
                    ..Default::default()
                },
            )
            .unwrap();
        drop(view);
        drop(mount);
        drop(filesystem);
        assert!(
            weak_owner.upgrade().is_some(),
            "an external path owns the real filesystem"
        );
        assert!(!retired.load(Ordering::Acquire));
        let metadata = path.location().metadata().unwrap();
        assert_eq!(
            (
                metadata.inode,
                metadata.uid,
                metadata.gid,
                metadata.mode.bits()
            ),
            (inode, 101, 202, 0o620)
        );
        drop(path);
        assert!(
            weak_owner.upgrade().is_none(),
            "the filesystem must not retain its own wrappers"
        );
        assert!(
            retired.load(Ordering::Acquire),
            "retiring the last owner retires its static backing"
        );
    }
}
