use alloc::{
    string::String,
    sync::{Arc, Weak},
};
use core::{any::Any, time::Duration};

use ax_lazyinit::LazyInit;
use ax_memory_addr::PAGE_SIZE_4K;
use axfs_ng_vfs::{
    AuthorizedMetadataChange, DeviceId, DirEntry, DirNode, Filesystem, FilesystemOps,
    LockedMetadataAttempt, Metadata, MetadataRequest, MetadataUpdate, NamespaceCompletion,
    NamespaceOutcome, NodeOps, NodePermission, NodeType, Reference, StatFs, VfsError, VfsResult,
    WeakDirEntry, WeakFilesystem, path::MAX_NAME_LEN,
};
use slab::Slab;

use super::DirMaker;
use crate::sync::{Mutex, MutexGuard, RawSpinLock};

/// Allocation domain retained by entities, never by a reverse filesystem edge.
struct SimpleFsCore {
    name: String,
    fs_type: u32,
    inodes: RawSpinLock<Slab<()>>,
}

/// Producer access to an instance's inode domain, without a filesystem receiver.
#[derive(Clone)]
pub struct SimpleFsContext {
    core: Arc<SimpleFsCore>,
}

impl SimpleFsContext {
    /// Identity remains valid while this context or one of its entities is retained.
    pub(crate) fn identity(&self) -> u64 {
        Arc::as_ptr(&self.core) as usize as u64
    }

    pub(super) fn same_instance(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.core, &other.core)
    }

    /// Reserves one inode lease, rejecting an unrepresentable number before insertion.
    pub fn allocate_inode(
        &self,
        node_type: NodeType,
        mode: NodePermission,
    ) -> VfsResult<Arc<SimpleInode>> {
        let (key, ino) = {
            let mut inodes = self.core.inodes.lock_irqsave();
            let key = inodes.vacant_key();
            let ino = u64::try_from(key)
                .ok()
                .and_then(|key| key.checked_add(1))
                .ok_or(VfsError::NoMemory)?;
            inodes.insert(());
            (key, ino)
        };
        Ok(Arc::new(SimpleInode {
            context: self.clone(),
            key,
            metadata: RawSpinLock::new(Metadata {
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
                atime: Duration::ZERO,
                mtime: Duration::ZERO,
                ctime: Duration::ZERO,
            }),
        }))
    }
}

/// Stable inode attributes and the unique lease for their allocation slot.
pub struct SimpleInode {
    context: SimpleFsContext,
    key: usize,
    // Metadata access can occur with IRQs disabled; it must not sleep.
    pub(crate) metadata: RawSpinLock<Metadata>,
}

impl SimpleInode {
    pub(super) fn context(&self) -> SimpleFsContext {
        self.context.clone()
    }

    pub(crate) fn metadata(&self) -> Metadata {
        self.metadata.lock_irqsave().clone()
    }

    pub(crate) fn update_metadata(&self, update: MetadataUpdate) {
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
    }
}

impl Drop for SimpleInode {
    fn drop(&mut self) {
        self.context.core.inodes.lock_irqsave().remove(self.key);
    }
}

/// Closed ownership of the actual receiver and its one shared VFS mount state.
#[derive(Clone)]
pub struct SimpleFsOwner {
    fs: Arc<SimpleFs>,
    filesystem: Filesystem,
}

impl AsRef<SimpleFs> for SimpleFsOwner {
    fn as_ref(&self) -> &SimpleFs {
        &self.fs
    }
}

impl SimpleFsOwner {
    /// Clones the original generic handle, preserving filesystem identity and protection.
    pub fn as_filesystem(&self) -> Filesystem {
        self.filesystem.clone()
    }

    pub fn context(&self) -> SimpleFsContext {
        self.fs.context.clone()
    }
}

/// Filesystem state owns recipes and only weakly indexes active root wrappers.
pub struct SimpleFs {
    context: SimpleFsContext,
    root_recipe: DirMaker,
    root: RawSpinLock<Option<WeakDirEntry>>,
    self_weak: Weak<Self>,
    publication: LazyInit<WeakFilesystem>,
    namespace: Mutex<()>,
}

impl SimpleFs {
    /// Runs the producer once and validates its root before publishing the receiver.
    pub fn new_with(
        name: String,
        fs_type: u32,
        builder: impl FnOnce(SimpleFsContext) -> VfsResult<DirMaker>,
    ) -> VfsResult<SimpleFsOwner> {
        let context = SimpleFsContext {
            core: Arc::new(SimpleFsCore {
                name,
                fs_type,
                inodes: RawSpinLock::new(Slab::new()),
            }),
        };
        let root_recipe = builder(context.clone())?;
        root_recipe.check_context(&context)?;
        let fs = Arc::new_cyclic(|this| Self {
            context,
            root_recipe,
            root: RawSpinLock::new(None),
            self_weak: this.clone(),
            publication: LazyInit::new(),
            namespace: Mutex::new(()),
        });
        // Filesystem::new queries only native read-only state, never root_dir.
        // Initialize this non-owning pair before any owner escapes construction.
        let filesystem = Filesystem::new(fs.clone());
        fs.publication.init_once(filesystem.downgrade());
        Ok(SimpleFsOwner { fs, filesystem })
    }

    fn owner(&self) -> SimpleFsOwner {
        SimpleFsOwner {
            fs: self
                .self_weak
                .upgrade()
                .expect("borrowed published filesystem"),
            filesystem: self
                .publication
                .upgrade()
                .expect("published filesystem mount state"),
        }
    }
}

impl FilesystemOps for SimpleFs {
    fn name(&self) -> &str {
        &self.context.core.name
    }

    fn root_dir(&self) -> DirEntry {
        let existing = self
            .root
            .lock_irqsave()
            .as_ref()
            .and_then(WeakDirEntry::upgrade);
        if let Some(root) = existing {
            return root;
        }
        let prepared = self
            .root_recipe
            .prepare(self.owner())
            .expect("validated root recipe domain");
        let candidate = DirEntry::new_dir(
            |this| DirNode::new(prepared.materialize(this)),
            Reference::root(),
        );
        let root = {
            let mut slot = self.root.lock_irqsave();
            if let Some(root) = slot.as_ref().and_then(WeakDirEntry::upgrade) {
                root
            } else {
                *slot = Some(candidate.downgrade());
                candidate.clone()
            }
        };
        // Both the winner and a losing candidate remain owned outside the index guard.
        drop(candidate);
        root
    }

    fn stat(&self) -> VfsResult<StatFs> {
        Ok(StatFs {
            fs_type: self.context.core.fs_type,
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

/// Active adapter retaining the actual receiver and its original entity attributes.
pub struct SimpleFsNode {
    owner: SimpleFsOwner,
    pub(crate) inode: Arc<SimpleInode>,
}

impl SimpleFsNode {
    pub(super) fn lock_namespace(&self) -> MutexGuard<'_, ()> {
        self.owner.fs.namespace.lock()
    }

    pub(crate) fn new(
        owner: SimpleFsOwner,
        node_type: NodeType,
        mode: NodePermission,
    ) -> VfsResult<Self> {
        let inode = owner.context().allocate_inode(node_type, mode)?;
        Ok(Self::bind(owner, inode))
    }

    /// Recipes verify the allocation domain before reaching this infallible assembly.
    pub(super) fn bind(owner: SimpleFsOwner, inode: Arc<SimpleInode>) -> Self {
        Self { owner, inode }
    }

    pub(super) fn owner(&self) -> SimpleFsOwner {
        self.owner.clone()
    }
}

impl NodeOps for SimpleFsNode {
    fn inode(&self) -> u64 {
        self.inode.metadata.lock_irqsave().inode
    }

    fn metadata(&self) -> VfsResult<Metadata> {
        Ok(self.inode.metadata())
    }

    fn len(&self) -> VfsResult<u64> {
        Ok(0)
    }

    fn update_metadata(&self, update: MetadataUpdate) -> VfsResult<()> {
        self.inode.update_metadata(update);
        Ok(())
    }

    fn change_metadata(&self, request: &MetadataRequest<'_>) -> NamespaceCompletion<Metadata> {
        if let Err(error) = request.admission().check_owner(self.owner.as_ref()) {
            return NamespaceCompletion::new(NamespaceOutcome::unchanged(Err(error)), None);
        }
        let outcome = {
            let mut metadata = self.inode.metadata.lock_irqsave();
            request.execute(&mut SimpleMetadataAttempt {
                metadata: &mut metadata,
            })
        };
        NamespaceCompletion::new(outcome, None)
    }

    fn filesystem(&self) -> &dyn FilesystemOps {
        self.owner.as_ref()
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
    use crate::pseudofs::{DirMapping, SimpleDir, SimpleFileBacking};

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
        let owner = SimpleFs::new_with("simple-owner-test".into(), 0x53494d50, move |fs| {
            let mut children = DirMapping::new();
            children.add(
                "retained",
                SimpleFileBacking::new_regular(fs.clone(), move || {
                    let _ = &backing_retirement;
                    Ok("retained object contents")
                })?,
            );
            SimpleDir::new_maker(fs, Arc::new(children))
        })
        .unwrap();
        let filesystem = owner.as_filesystem();
        let weak_owner = Arc::downgrade(&owner.fs);
        drop(owner);
        let mount = Mountpoint::new_root(&filesystem);
        let context = FsContext::from_tree(mount.clone(), mount.root_location()).unwrap();
        let caller = NativeLinkCaller::root();
        let view = FsView::native(context, &caller);
        let path = view.resolve("/retained").unwrap();
        let inode = path.location().metadata().unwrap().inode;
        let root_inode = mount.root_location().metadata().unwrap().inode;
        let weak_root = mount.root_location().entry().downgrade();
        let weak_file = path.location().entry().downgrade();
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
        TrustedInitialization::kernel()
            .restore_metadata(
                &mount.root_location(),
                MetadataUpdate {
                    mode: Some(NodePermission::from_bits_truncate(0o701)),
                    owner: Some((303, 404)),
                    ..Default::default()
                },
            )
            .unwrap();
        drop(path);
        drop(view);
        mount
            .root_location()
            .entry()
            .as_dir()
            .unwrap()
            .clear_cached_entries();
        drop(mount);
        assert!(
            weak_file.upgrade().is_none(),
            "inactive file wrappers retire"
        );
        assert!(
            weak_root.upgrade().is_none(),
            "inactive root wrappers retire"
        );
        assert!(!retired.load(Ordering::Acquire));

        let mount = Mountpoint::new_root(&filesystem);
        let root_metadata = mount.root_location().metadata().unwrap();
        assert_eq!(
            (
                root_metadata.inode,
                root_metadata.uid,
                root_metadata.gid,
                root_metadata.mode.bits()
            ),
            (root_inode, 303, 404, 0o701)
        );
        let context = FsContext::from_tree(mount.clone(), mount.root_location()).unwrap();
        let view = FsView::native(context, &caller);
        let path = view.resolve("/retained").unwrap();
        drop(view);
        mount
            .root_location()
            .entry()
            .as_dir()
            .unwrap()
            .clear_cached_entries();
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
