use alloc::{
    borrow::{Cow, ToOwned},
    string::String,
    sync::{Arc, Weak},
    vec,
    vec::Vec,
};
use core::{
    any::Any,
    iter, mem,
    sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    task::Context,
    time::Duration,
};

use hashbrown::HashMap;
use inherit_methods_macro::inherit_methods;
use log::warn;

use crate::{
    DeviceId, DirEntry, DirEntrySink, DirNode, DirNodeOps, Filesystem, FilesystemOps, FsIoEvents,
    FsPollable, Metadata, MetadataUpdate, Mutex, MutexGuard, NodeFlags, NodeOps, NodePermission,
    NodeType, OpenOptions, Reference, ReferenceKey, TypeMap, VfsError, VfsResult, WeakDirEntry,
    path::{DOT, DOTDOT, PathBuf, verify_entry_name},
};

static DEVICE_COUNTER: AtomicU64 = AtomicU64::new(1);
static MOUNT_ID_COUNTER: AtomicU64 = AtomicU64::new(1);
static PEER_GROUP_COUNTER: AtomicU64 = AtomicU64::new(1);
static SYNTHETIC_MOUNT_INODE_COUNTER: AtomicU64 = AtomicU64::new(1_u64 << 63);

struct SyntheticMountDir {
    parent: DirEntry,
    this: WeakDirEntry,
    inode: u64,
    mode: NodePermission,
    uid: u32,
    gid: u32,
}

impl SyntheticMountDir {
    fn new(parent: DirEntry, this: WeakDirEntry, mode: NodePermission, uid: u32, gid: u32) -> Self {
        Self {
            parent,
            this,
            inode: SYNTHETIC_MOUNT_INODE_COUNTER.fetch_add(1, Ordering::Relaxed),
            mode,
            uid,
            gid,
        }
    }
}

impl NodeOps for SyntheticMountDir {
    fn inode(&self) -> u64 {
        self.inode
    }

    fn metadata(&self) -> VfsResult<Metadata> {
        Ok(Metadata {
            device: 0,
            inode: self.inode,
            nlink: 2,
            mode: self.mode,
            node_type: NodeType::Directory,
            uid: self.uid,
            gid: self.gid,
            size: 0,
            block_size: 0,
            blocks: 0,
            rdev: DeviceId::default(),
            atime: Duration::ZERO,
            mtime: Duration::ZERO,
            ctime: Duration::ZERO,
        })
    }

    fn update_metadata(&self, _update: MetadataUpdate) -> VfsResult<()> {
        Err(VfsError::ReadOnlyFilesystem)
    }

    fn filesystem(&self) -> &dyn FilesystemOps {
        self.parent.filesystem()
    }

    fn sync(&self, _data_only: bool) -> VfsResult<()> {
        Ok(())
    }

    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }
}

impl DirNodeOps for SyntheticMountDir {
    fn read_dir(&self, offset: u64, sink: &mut dyn DirEntrySink) -> VfsResult<usize> {
        let entries = [
            (DOT, self.inode, NodeType::Directory),
            (DOTDOT, self.parent.inode(), NodeType::Directory),
        ];
        let start = usize::try_from(offset).unwrap_or(usize::MAX);
        let mut count = 0;
        for (index, (name, ino, node_type)) in entries.iter().enumerate().skip(start) {
            if !sink.accept(name, *ino, *node_type, (index + 1) as u64) {
                break;
            }
            count += 1;
        }
        Ok(count)
    }

    fn lookup(&self, name: &str) -> VfsResult<DirEntry> {
        match name {
            DOT => self.this.upgrade().ok_or(VfsError::NotFound),
            DOTDOT => Ok(self.parent.clone()),
            _ => Err(VfsError::NotFound),
        }
    }

    fn create(
        &self,
        _name: &str,
        _node_type: NodeType,
        _permission: NodePermission,
        _uid: u32,
        _gid: u32,
    ) -> VfsResult<DirEntry> {
        Err(VfsError::ReadOnlyFilesystem)
    }

    fn link(&self, _name: &str, _node: &DirEntry) -> VfsResult<DirEntry> {
        Err(VfsError::ReadOnlyFilesystem)
    }

    fn unlink(&self, _name: &str, _is_dir: bool) -> VfsResult<()> {
        Err(VfsError::ReadOnlyFilesystem)
    }

    fn rename(&self, _src_name: &str, _dst_dir: &DirNode, _dst_name: &str) -> VfsResult<()> {
        Err(VfsError::ReadOnlyFilesystem)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PropagationType {
    Private,
    Shared,
    Slave,
    Unbindable,
}

#[derive(Debug)]
pub struct Mountpoint {
    /// Root dir entry in the mountpoint.
    root: DirEntry,
    /// Location in the parent mountpoint. `None` for the global root mount.
    location: Mutex<Option<Location>>,
    /// Children of the mountpoint in this namespace-local mount tree.
    children: Mutex<HashMap<ReferenceKey, Arc<Self>>>,
    /// Device ID (filesystem superblock device — used for major:minor in mountinfo).
    device: u64,
    /// Unique mount identifier (Linux `mnt_id`), assigned from `MOUNT_ID_COUNTER`.
    /// Distinct from `device` which is the filesystem's device number.
    mount_id: u64,
    /// Peer group ID for shared mounts (Linux `mnt_group_id`). 0 = not shared.
    /// Assigned when `set_shared()` is first called; shared among all mounts in
    /// the same peer group.
    peer_group_id: AtomicU64,
    /// Read-only flag for this mountpoint.
    readonly: AtomicBool,
    /// Mount option flags (Linux MS_* bits: MS_NOSUID=2, MS_NODEV=4,
    /// MS_NOEXEC=8, MS_NOATIME=0x400, MS_RELATIME=0x800000,
    /// MS_STRICTATIME=0x1000000). MS_RDONLY is tracked separately via
    /// `readonly` for backward compatibility.
    mount_flags: AtomicU32,
    /// Expire mark for umount2(MNT_EXPIRE).
    expired: AtomicBool,
    /// Mount propagation type.
    propagation: Mutex<PropagationType>,
    /// Other shared peers in the same propagation group.
    peers: Mutex<Vec<Weak<Self>>>,
    /// Slave mounts that receive propagation events from this shared mount.
    slaves: Mutex<Vec<Weak<Self>>>,
    /// Shared masters that this slave receives propagation events from.
    masters: Mutex<Vec<Weak<Self>>>,
}

impl Mountpoint {
    fn new_with_root(
        root: DirEntry,
        location_in_parent: Option<Location>,
        device: u64,
    ) -> Arc<Self> {
        Arc::new(Self {
            root,
            location: Mutex::new(location_in_parent),
            children: Mutex::new(HashMap::default()),
            device,
            mount_id: MOUNT_ID_COUNTER.fetch_add(1, Ordering::Relaxed),
            peer_group_id: AtomicU64::new(0),
            readonly: AtomicBool::new(false),
            mount_flags: AtomicU32::new(0),
            expired: AtomicBool::new(false),
            propagation: Mutex::new(PropagationType::Private),
            peers: Mutex::default(),
            slaves: Mutex::default(),
            masters: Mutex::default(),
        })
    }

    pub fn new(fs: &Filesystem, location_in_parent: Option<Location>) -> Arc<Self> {
        let result = Self::new_with_root(
            fs.root_dir(),
            location_in_parent,
            DEVICE_COUNTER.fetch_add(1, Ordering::Relaxed),
        );
        result.readonly.store(fs.is_readonly(), Ordering::Release);
        result
    }

    pub fn new_root(fs: &Filesystem) -> Arc<Self> {
        Self::new(fs, None)
    }

    fn bind(source: &Location, location_in_parent: Location, recursive: bool) -> Arc<Self> {
        let result = Self::new_with_root(
            source.entry.clone(),
            Some(location_in_parent),
            source.mountpoint.device(),
        );
        result
            .readonly
            .store(source.mountpoint.is_readonly(), Ordering::Release);
        if recursive {
            Self::clone_children_from(&source.mountpoint, &result, true);
        }
        result
    }

    fn clone_shallow(source: &Arc<Self>, location_in_parent: Option<Location>) -> Arc<Self> {
        let result = Self::new_with_root(source.root.clone(), location_in_parent, source.device());
        result
            .readonly
            .store(source.is_readonly(), Ordering::Release);
        result
            .mount_flags
            .store(source.mount_flags(), Ordering::Release);
        result
            .peer_group_id
            .store(source.peer_group_id(), Ordering::Release);
        *result.propagation.lock() = source.propagation();
        result
            .expired
            .store(source.expired.load(Ordering::Acquire), Ordering::Release);
        result
    }

    fn clone_children_from(source: &Arc<Self>, target: &Arc<Self>, skip_unbindable: bool) {
        let children: Vec<_> = source
            .children
            .lock()
            .iter()
            .map(|(key, child)| (key.clone(), child.clone()))
            .filter(|(_, child)| !(skip_unbindable && child.is_unbindable()))
            .collect();

        let mut target_children = target.children.lock();
        for (key, child) in children {
            let location = child
                .location
                .lock()
                .as_ref()
                .map(|loc| Location::new(target.clone(), loc.entry.clone()));
            let cloned = Self::clone_shallow(&child, location);
            Self::clone_children_from(&child, &cloned, skip_unbindable);
            target_children.insert(key, cloned);
        }
    }

    /// Clone this mount tree into an independent namespace-local topology.
    ///
    /// The returned tree shares underlying directory entries and filesystem
    /// objects, but all `Mountpoint` nodes and parent/child links are private
    /// to the clone.
    pub fn clone_tree(self: &Arc<Self>) -> Arc<Self> {
        let result = Self::clone_shallow(self, None);
        Self::clone_children_from(self, &result, false);
        result
    }

    pub fn root_location(self: &Arc<Self>) -> Location {
        Location::new(self.clone(), self.root.clone())
    }

    /// Returns live child mountpoints in this namespace-local mount tree.
    pub fn children(&self) -> Vec<Arc<Self>> {
        self.children.lock().values().cloned().collect()
    }

    /// Returns the location in the parent mountpoint.
    pub fn location(&self) -> Option<Location> {
        self.location.lock().clone()
    }

    pub fn is_root(&self) -> bool {
        self.location.lock().is_none()
    }

    /// Pivot the mount tree: the old root (`self`) is detached and re-attached
    /// at `put_old` under `new_root_mp`, which becomes the global root.
    ///
    /// This implements the mount-tree portion of Linux `pivot_root(2)`.
    pub fn pivot_mount(
        self: &Arc<Self>,        // old root mountpoint
        new_root_mp: &Arc<Self>, // new root mountpoint
        put_old: &Location,      // directory under new_root_mp where old root goes
    ) -> VfsResult<()> {
        // put_old must belong to the new root's mountpoint tree.
        if !Arc::ptr_eq(put_old.mountpoint(), new_root_mp) {
            return Err(VfsError::InvalidInput);
        }
        // put_old must be a directory and not already a mountpoint.
        put_old.check_is_dir()?;
        if put_old.is_mountpoint() {
            return Err(VfsError::ResourceBusy);
        }

        // 1. Detach new_root from old root's children and clear the old mount
        //    slot (where new_root was attached in the old root).
        {
            let mut new_root_loc = new_root_mp.location.lock();
            if let Some(ref old_loc) = *new_root_loc {
                self.children.lock().remove(&old_loc.entry.key());
            }
            // new_root becomes the global root.
            *new_root_loc = None;
        }

        // 2. Attach old root at put_old under new_root.
        {
            new_root_mp
                .children
                .lock()
                .insert(put_old.entry.key(), self.clone());
            *self.location.lock() = Some(put_old.clone());
        }

        Ok(())
    }

    /// Returns the effective mountpoint.
    ///
    /// For example, first `mount /dev/sda1 /mnt` and then `mount /dev/sda2
    /// /mnt`. After the second mount is completed, the content of the first
    /// mount will be overridden (root mount -> mnt1 -> mnt2). We need to
    /// return `mnt2` for `mnt1.effective_mountpoint()`.
    pub(crate) fn effective_mountpoint(self: &Arc<Self>) -> Arc<Mountpoint> {
        let mut mountpoint = self.clone();
        while let Some(mount) = {
            mountpoint
                .children
                .lock()
                .get(&mountpoint.root.key())
                .cloned()
        } {
            mountpoint = mount;
        }
        mountpoint
    }

    pub fn device(self: &Arc<Self>) -> u64 {
        self.device
    }

    pub fn mount_id(&self) -> u64 {
        self.mount_id
    }

    pub fn peer_group_id(&self) -> u64 {
        self.peer_group_id.load(Ordering::Acquire)
    }

    /// For slave mounts: returns the peer group ID of the first master.
    /// Used for mountinfo `master:N` field.
    pub fn first_master_peer_group_id(&self) -> Option<u64> {
        self.masters
            .lock()
            .iter()
            .filter_map(|weak| weak.upgrade())
            .next()
            .map(|m| m.peer_group_id())
            .filter(|id| *id != 0)
    }

    /// Walk the mount tree rooted at `self`, collecting `(mount_id, parent_id,
    /// mountpoint)` tuples in DFS order.
    ///
    /// `mount_id` is the mount's [`device()`](Self::device) (unique per mount,
    /// assigned incrementally from `DEVICE_COUNTER` — the root mount is 1).
    /// `parent_id` for the root mount is itself (Linux convention:
    /// `mount_id == parent_id` for the root mount); for non-root mounts it is
    /// the parent mount's `device()`.
    ///
    /// Lock safety: children are collected into a `Vec` by cloning the `Arc`s
    /// outside the lock before recursion, so no `Mutex` guard is held during
    /// the recursive call.
    pub fn walk_tree(self: &Arc<Self>) -> Vec<(u64, u64, Arc<Mountpoint>)> {
        let mut result = Vec::new();
        self.walk_tree_inner(&mut result);
        result
    }

    fn walk_tree_inner(self: &Arc<Self>, result: &mut Vec<(u64, u64, Arc<Mountpoint>)>) {
        let mount_id = self.mount_id();
        // Root mount (location == None) is its own parent per Linux convention.
        let parent_id = self
            .location
            .lock()
            .as_ref()
            .map_or(mount_id, |loc| loc.mountpoint().mount_id());
        result.push((mount_id, parent_id, self.clone()));

        // Collect children outside the lock to avoid holding it during recursion.
        let children: Vec<Arc<Self>> = self.children.lock().values().cloned().collect();
        for child in children {
            child.walk_tree_inner(result);
        }
    }

    pub fn is_readonly(&self) -> bool {
        self.readonly.load(Ordering::Acquire)
    }

    pub fn set_readonly(&self, readonly: bool) {
        self.readonly.store(readonly, Ordering::Release);
    }

    pub fn mount_flags(&self) -> u32 {
        self.mount_flags.load(Ordering::Acquire)
    }

    pub fn set_mount_flags(&self, flags: u32) {
        self.mount_flags.store(flags, Ordering::Release);
    }

    pub fn mark_expired(&self) -> bool {
        self.expired.swap(true, Ordering::AcqRel)
    }

    pub fn clear_expired(&self) {
        self.expired.store(false, Ordering::Release);
    }

    fn propagation(&self) -> PropagationType {
        *self.propagation.lock()
    }

    pub fn is_shared(&self) -> bool {
        self.propagation() == PropagationType::Shared
    }

    pub fn is_slave(&self) -> bool {
        self.propagation() == PropagationType::Slave
    }

    pub fn is_unbindable(&self) -> bool {
        self.propagation() == PropagationType::Unbindable
    }

    fn remove_from_shared_group(self: &Arc<Self>) {
        let peers: Vec<_> = self.peers.lock().iter().filter_map(Weak::upgrade).collect();
        for peer in peers {
            peer.peers.lock().retain(|candidate| {
                candidate
                    .upgrade()
                    .is_some_and(|mp| !Arc::ptr_eq(&mp, self))
            });
        }
        self.peers.lock().clear();
    }

    fn remove_from_masters(self: &Arc<Self>) {
        let masters: Vec<_> = self
            .masters
            .lock()
            .iter()
            .filter_map(Weak::upgrade)
            .collect();
        for master in masters {
            master.slaves.lock().retain(|candidate| {
                candidate
                    .upgrade()
                    .is_some_and(|mp| !Arc::ptr_eq(&mp, self))
            });
        }
        self.masters.lock().clear();
    }

    pub fn set_shared(self: &Arc<Self>) {
        self.remove_from_shared_group();
        self.remove_from_masters();
        *self.propagation.lock() = PropagationType::Shared;
        if self.peer_group_id.load(Ordering::Acquire) == 0 {
            self.peer_group_id.store(
                PEER_GROUP_COUNTER.fetch_add(1, Ordering::Relaxed),
                Ordering::Release,
            );
        }
    }

    pub fn set_private(self: &Arc<Self>) {
        self.remove_from_shared_group();
        self.remove_from_masters();
        *self.propagation.lock() = PropagationType::Private;
        self.peer_group_id.store(0, Ordering::Release);
    }

    pub fn set_slave(self: &Arc<Self>) {
        let mut masters = Vec::new();
        if self.is_shared() {
            masters.extend(self.peers.lock().iter().filter_map(Weak::upgrade));
        }

        self.remove_from_shared_group();
        self.remove_from_masters();
        *self.propagation.lock() = PropagationType::Slave;
        self.peer_group_id.store(0, Ordering::Release);
        for master in masters {
            master.slaves.lock().push(Arc::downgrade(self));
            self.masters.lock().push(Arc::downgrade(&master));
        }
    }

    pub fn set_unbindable(self: &Arc<Self>) {
        self.set_private();
        *self.propagation.lock() = PropagationType::Unbindable;
    }

    pub fn join_shared_group(self: &Arc<Self>, source: &Arc<Self>) {
        let mut group = vec![source.clone()];
        group.extend(source.peers.lock().iter().filter_map(Weak::upgrade));

        self.set_shared();
        let source_group = source.peer_group_id.load(Ordering::Acquire);
        if source_group != 0 {
            self.peer_group_id.store(source_group, Ordering::Release);
        }
        for member in group {
            if Arc::ptr_eq(&member, self) {
                continue;
            }
            member.peers.lock().push(Arc::downgrade(self));
            self.peers.lock().push(Arc::downgrade(&member));
        }
    }

    fn attach_child(parent: &Arc<Self>, location: Location, child: &Arc<Self>) -> VfsResult<()> {
        location.check_is_dir()?;
        parent
            .children
            .lock()
            .insert(location.entry.key(), child.clone());
        Ok(())
    }

    fn propagate_new_child(
        source_parent: &Arc<Self>,
        source_location: &Location,
        child: &Arc<Self>,
    ) -> VfsResult<()> {
        let peers: Vec<_> = source_parent
            .peers
            .lock()
            .iter()
            .filter_map(Weak::upgrade)
            .collect();
        let slaves: Vec<_> = source_parent
            .slaves
            .lock()
            .iter()
            .filter_map(Weak::upgrade)
            .collect();
        let mut path_components = vec![];
        let mut current = source_location.clone();
        while !current.is_root_of_mount() {
            path_components.push(current.name().into_owned());
            current = current.parent().ok_or(VfsError::InvalidInput)?;
        }
        path_components.reverse();

        for target_parent in peers.into_iter().chain(slaves) {
            let mut location = target_parent.root_location();
            for component in &path_components {
                location = location.lookup_no_follow(component)?;
            }
            let inserted_key = location.entry.key();
            Self::attach_child(&target_parent, location, child)?;
            let mut resolved = target_parent.root_location();
            for component in &path_components {
                resolved = resolved.lookup_no_follow(component)?;
            }
            if !Arc::ptr_eq(resolved.mountpoint(), child) {
                warn!(
                    "mount propagation mismatch path={:?} inserted_key={:?} resolved_key={:?} \
                     resolved_is_root={} resolved_mp_device={} replicated_device={}",
                    path_components,
                    inserted_key,
                    resolved.entry.key(),
                    resolved.is_root_of_mount(),
                    resolved.mountpoint().device(),
                    child.device(),
                );
            }
        }
        Ok(())
    }

    pub fn move_to(self: &Arc<Self>, new_location: &Location) -> VfsResult<()> {
        if self.is_root() {
            return Err(VfsError::InvalidInput);
        }
        if new_location.is_mountpoint() {
            return Err(VfsError::ResourceBusy);
        }
        new_location.check_is_dir()?;
        let root_location = self.root_location();
        let mut current = Some(new_location.clone());
        while let Some(location) = current {
            if location.ptr_eq(&root_location) {
                return Err(VfsError::FilesystemLoop);
            }
            current = location.parent();
        }

        let Some(old_location) = self.location.lock().clone() else {
            return Err(VfsError::InvalidInput);
        };

        old_location
            .mountpoint
            .children
            .lock()
            .remove(&old_location.entry.key());

        new_location
            .mountpoint
            .children
            .lock()
            .insert(new_location.entry.key(), self.clone());

        *self.location.lock() = Some(new_location.clone());
        Ok(())
    }

    pub fn detach(self: &Arc<Self>) -> VfsResult<()> {
        if self.is_root() {
            return Err(VfsError::InvalidInput);
        }
        let Some(location) = self.location.lock().clone() else {
            return Err(VfsError::InvalidInput);
        };
        location
            .mountpoint
            .children
            .lock()
            .remove(&location.entry.key());
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct Location {
    mountpoint: Arc<Mountpoint>,
    entry: DirEntry,
}

#[inherit_methods(from = "self.entry")]
impl Location {
    pub fn inode(&self) -> u64;

    pub fn filesystem(&self) -> &dyn FilesystemOps;

    #[allow(clippy::len_without_is_empty)]
    pub fn len(&self) -> VfsResult<u64>;

    pub fn sync(&self, data_only: bool) -> VfsResult<()>;

    pub fn is_file(&self) -> bool;

    pub fn is_dir(&self) -> bool;

    pub fn node_type(&self) -> NodeType;

    pub fn read_link(&self) -> VfsResult<String>;

    pub fn ioctl(&self, cmd: u32, arg: usize) -> VfsResult<usize>;

    pub fn flags(&self) -> NodeFlags;

    pub fn user_data(&self) -> MutexGuard<'_, TypeMap>;
}

impl Location {
    pub fn new(mountpoint: Arc<Mountpoint>, entry: DirEntry) -> Self {
        Self { mountpoint, entry }
    }

    fn wrap(&self, entry: DirEntry) -> Self {
        Self::new(self.mountpoint.clone(), entry)
    }

    pub fn mountpoint(&self) -> &Arc<Mountpoint> {
        &self.mountpoint
    }

    pub fn is_readonly(&self) -> bool {
        self.mountpoint.is_readonly()
    }

    pub fn entry(&self) -> &DirEntry {
        &self.entry
    }

    pub fn is_root_of_mount(&self) -> bool {
        self.entry.ptr_eq(&self.mountpoint.root)
    }

    pub fn update_metadata(&self, update: MetadataUpdate) -> VfsResult<()> {
        if self.is_readonly() {
            return Err(VfsError::ReadOnlyFilesystem);
        }
        self.entry.update_metadata(update)
    }

    /// Returns the entry name.
    ///
    /// For mount roots the name is derived from the parent location (where this
    /// mount was attached). Because `location` lives behind a `Mutex`, the
    /// mount-root case returns an owned `Cow::Owned`; the common non-root case
    /// returns a borrowed `Cow::Borrowed`.
    pub fn name(&self) -> Cow<'_, str> {
        if self.is_root_of_mount() {
            self.mountpoint
                .location
                .lock()
                .as_ref()
                .map_or(Cow::Borrowed(""), |loc| Cow::Owned(loc.name().into_owned()))
        } else {
            Cow::Borrowed(self.entry.name())
        }
    }

    pub fn parent(&self) -> Option<Self> {
        if !self.is_root_of_mount() {
            return Some(self.wrap(self.entry.parent().unwrap()));
        }
        self.mountpoint.location()?.parent()
    }

    pub fn is_root(&self) -> bool {
        self.mountpoint.is_root() && self.is_root_of_mount()
    }

    pub fn check_is_dir(&self) -> VfsResult<()> {
        self.entry.as_dir().map(|_| ())
    }

    pub fn check_is_file(&self) -> VfsResult<()> {
        self.entry.as_file().map(|_| ())
    }

    pub fn metadata(&self) -> VfsResult<Metadata> {
        let mut metadata = self.entry.metadata()?;
        metadata.device = self.mountpoint.device();
        Ok(metadata)
    }

    pub fn absolute_path(&self) -> VfsResult<PathBuf> {
        let mut components = vec![];
        let mut cur = self.clone();
        loop {
            cur.entry.collect_absolute_path(&mut components);
            cur = match cur.mountpoint.location() {
                Some(loc) => loc,
                None => break,
            }
        }
        Ok(iter::once("/")
            .chain(components.iter().map(String::as_str).rev())
            .collect())
    }

    pub fn ptr_eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.mountpoint, &other.mountpoint) && self.entry.ptr_eq(&other.entry)
    }

    pub fn is_mountpoint(&self) -> bool {
        self.entry.as_dir().is_ok()
            && self
                .mountpoint
                .children
                .lock()
                .contains_key(&self.entry.key())
    }

    /// See [`Mountpoint::effective_mountpoint`].
    fn resolve_mountpoint(self) -> Self {
        let Some(mountpoint) = self
            .mountpoint
            .children
            .lock()
            .get(&self.entry.key())
            .cloned()
        else {
            return self;
        };
        let mountpoint = mountpoint.effective_mountpoint();
        let entry = mountpoint.root.clone();
        Self::new(mountpoint, entry)
    }

    pub fn lookup_no_follow(&self, name: &str) -> VfsResult<Self> {
        Ok(match name {
            DOT => self.clone(),
            DOTDOT => self.parent().unwrap_or_else(|| self.clone()),
            _ => {
                let loc = Self::new(self.mountpoint.clone(), self.entry.as_dir()?.lookup(name)?);
                loc.resolve_mountpoint()
            }
        })
    }

    pub fn create(
        &self,
        name: &str,
        node_type: NodeType,
        permission: NodePermission,
        uid: u32,
        gid: u32,
    ) -> VfsResult<Self> {
        if self.is_readonly() {
            return Err(VfsError::ReadOnlyFilesystem);
        }
        self.entry
            .as_dir()?
            .create(name, node_type, permission, uid, gid)
            .map(|entry| self.wrap(entry))
    }

    /// Creates an in-memory directory entry that exists only as a mount target.
    ///
    /// This is intended for early boot auto-mount recovery: if the root
    /// filesystem is forced read-only because its on-disk state is dirty or
    /// inconsistent, other partitions still need stable mount targets such as
    /// `/boot` or `/userdata`. The placeholder is inserted only into the parent
    /// dentry cache and does not mutate the backing filesystem. If the backing
    /// filesystem has a non-directory entry with the same name, this helper
    /// deliberately shadows it so the mount can cover the bad root entry.
    pub fn create_transient_mount_dir(
        &self,
        name: &str,
        permission: NodePermission,
        uid: u32,
        gid: u32,
    ) -> VfsResult<Self> {
        verify_entry_name(name)?;
        if !self.is_readonly() {
            return Err(VfsError::InvalidInput);
        }
        let dir = self.entry.as_dir()?;
        if let Some(entry) = dir.lookup_cache(name)
            && entry.node_type() == NodeType::Directory
        {
            return Ok(self.wrap(entry).resolve_mountpoint());
        }
        match dir.lookup(name) {
            Ok(entry) if entry.node_type() == NodeType::Directory => {
                return Ok(self.wrap(entry).resolve_mountpoint());
            }
            Ok(_) => {}
            Err(err) if err.canonicalize() == VfsError::NotFound => {}
            Err(err) => return Err(err),
        }

        let parent = self.entry.clone();
        let reference = Reference::new(Some(parent.clone()), name.to_owned());
        let entry = DirEntry::new_dir(
            |this| {
                DirNode::new(Arc::new(SyntheticMountDir::new(
                    parent, this, permission, uid, gid,
                )))
            },
            reference,
        );
        dir.insert_cache(name.to_owned(), entry.clone());
        Ok(self.wrap(entry))
    }

    pub fn link(&self, name: &str, node: &Self) -> VfsResult<Self> {
        if self.is_readonly() {
            return Err(VfsError::ReadOnlyFilesystem);
        }
        if !Arc::ptr_eq(&self.mountpoint, &node.mountpoint) {
            return Err(VfsError::CrossesDevices);
        }
        self.entry
            .as_dir()?
            .link(name, &node.entry)
            .map(|entry| self.wrap(entry))
    }

    pub fn rename(&self, src_name: &str, dst_dir: &Self, dst_name: &str) -> VfsResult<()> {
        if self.is_readonly() || dst_dir.is_readonly() {
            return Err(VfsError::ReadOnlyFilesystem);
        }
        if !Arc::ptr_eq(&self.mountpoint, &dst_dir.mountpoint) {
            return Err(VfsError::CrossesDevices);
        }
        // Disallow moving a directory into one of its own descendants. Regular
        // files may still be renamed into child directories (e.g. Redis AOF
        // `temp-rewriteaof-*.aof` -> `appendonlydir/...`).
        if let Ok(src_loc) = self.lookup_no_follow(src_name)
            && src_loc.node_type() == NodeType::Directory
            && !self.ptr_eq(dst_dir)
            && src_loc.entry.is_ancestor_of(&dst_dir.entry)?
        {
            return Err(VfsError::InvalidInput);
        }
        self.entry
            .as_dir()?
            .rename(src_name, dst_dir.entry.as_dir()?, dst_name)
    }

    pub fn unlink(&self, name: &str, is_dir: bool) -> VfsResult<()> {
        if self.is_readonly() {
            return Err(VfsError::ReadOnlyFilesystem);
        }
        self.entry.as_dir()?.unlink(name, is_dir)
    }

    pub fn open_file(&self, name: &str, options: &OpenOptions) -> VfsResult<Location> {
        if self.is_readonly() && (options.create || options.create_new) {
            return Err(VfsError::ReadOnlyFilesystem);
        }
        self.entry
            .as_dir()?
            .open_file(name, options)
            .map(|entry| self.wrap(entry).resolve_mountpoint())
    }

    pub fn read_dir(&self, offset: u64, sink: &mut dyn DirEntrySink) -> VfsResult<usize> {
        self.entry.as_dir()?.read_dir(offset, sink)
    }

    pub fn mount(&self, fs: &Filesystem) -> VfsResult<Arc<Mountpoint>> {
        let result = Mountpoint::new(fs, Some(self.clone()));
        let should_propagate = self.mountpoint.is_shared();
        self.check_is_dir()?;
        {
            let mut children = self.mountpoint.children.lock();
            if children.contains_key(&self.entry.key()) {
                return Err(VfsError::ResourceBusy);
            }
            children.insert(self.entry.key(), result.clone());
        }
        if should_propagate {
            Mountpoint::propagate_new_child(self.mountpoint(), self, &result)?;
        }
        Ok(result)
    }

    pub fn bind_mount(&self, source: &Self, recursive: bool) -> VfsResult<Arc<Mountpoint>> {
        if source.mountpoint().is_unbindable() {
            return Err(VfsError::InvalidInput);
        }

        self.check_is_dir()?;
        let mut children = self.mountpoint.children.lock();
        if children.contains_key(&self.entry.key()) {
            return Err(VfsError::ResourceBusy);
        }
        let result = Mountpoint::bind(source, self.clone(), recursive);
        if source.mountpoint().is_shared() {
            result.join_shared_group(source.mountpoint());
        } else if source.mountpoint().is_slave() {
            result.set_slave();
        }
        children.insert(self.entry.key(), result.clone());
        Ok(result)
    }

    pub fn move_mount(&self, target: &Self) -> VfsResult<()> {
        if !self.is_root_of_mount() {
            return Err(VfsError::InvalidInput);
        }
        self.mountpoint.move_to(target)
    }

    pub fn unmount(&self) -> VfsResult<()> {
        if !self.is_root_of_mount() {
            return Err(VfsError::InvalidInput);
        }
        if !self.mountpoint.children.lock().is_empty() {
            return Err(VfsError::ResourceBusy);
        }
        assert!(self.entry.ptr_eq(&self.mountpoint.root));

        // Flush filesystem metadata (superblock, bitmaps, etc.) to the
        // backing block device before tearing down the mount.  For ext4
        // this writes a clean superblock so the next mount does not see
        // s_state = ERROR_FS.  For tmpfs/ramfs the default flush is a
        // no-op.
        self.filesystem().flush()?;
        self.mountpoint.clear_expired();

        self.entry.as_dir()?.forget();
        if let Some(parent_loc) = self.mountpoint.location.lock().as_ref() {
            parent_loc
                .mountpoint
                .children
                .lock()
                .remove(&parent_loc.entry.key());
        }
        Ok(())
    }

    pub fn detach_mount(&self) -> VfsResult<()> {
        if !self.is_root_of_mount() {
            return Err(VfsError::InvalidInput);
        }
        self.mountpoint.detach()
    }

    pub fn unmount_all(&self) -> VfsResult<()> {
        if !self.is_root_of_mount() {
            return Err(VfsError::InvalidInput);
        }
        let children = mem::take(&mut *self.mountpoint.children.lock());
        for (_, child) in children {
            child.root_location().unmount_all()?;
        }
        self.unmount()
    }
}

#[inherit_methods(from = "self.entry")]
impl FsPollable for Location {
    fn poll(&self) -> FsIoEvents;

    fn register(&self, context: &mut Context<'_>, events: FsIoEvents);
}

#[cfg(test)]
mod tests {
    use alloc::string::ToString;
    use core::any::Any;

    use super::*;
    use crate::StatFs;

    struct MockFs;
    struct MockNode;

    static MOCK_FS: MockFs = MockFs;

    impl FilesystemOps for MockFs {
        fn name(&self) -> &str {
            "mock"
        }
        fn root_dir(&self) -> DirEntry {
            let node: Arc<dyn DirNodeOps> = Arc::new(MockNode);
            DirEntry::new_dir(|_| DirNode::new(node), Reference::root())
        }
        fn stat(&self) -> VfsResult<StatFs> {
            Err(VfsError::InvalidInput)
        }
    }

    impl NodeOps for MockNode {
        fn inode(&self) -> u64 {
            0
        }
        fn metadata(&self) -> VfsResult<Metadata> {
            Err(VfsError::InvalidInput)
        }
        fn update_metadata(&self, _update: MetadataUpdate) -> VfsResult<()> {
            Err(VfsError::InvalidInput)
        }
        fn filesystem(&self) -> &dyn FilesystemOps {
            &MOCK_FS
        }
        fn sync(&self, _data_only: bool) -> VfsResult<()> {
            Err(VfsError::InvalidInput)
        }
        fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
            self
        }
    }

    impl DirNodeOps for MockNode {
        fn read_dir(&self, _offset: u64, _sink: &mut dyn DirEntrySink) -> VfsResult<usize> {
            Ok(0)
        }
        fn lookup(&self, _name: &str) -> VfsResult<DirEntry> {
            Err(VfsError::NotFound)
        }
        fn create(
            &self,
            _name: &str,
            _node_type: NodeType,
            _permission: NodePermission,
            _uid: u32,
            _gid: u32,
        ) -> VfsResult<DirEntry> {
            Err(VfsError::ReadOnlyFilesystem)
        }
        fn link(&self, _name: &str, _node: &DirEntry) -> VfsResult<DirEntry> {
            Err(VfsError::ReadOnlyFilesystem)
        }
        fn unlink(&self, _name: &str, _is_dir: bool) -> VfsResult<()> {
            Err(VfsError::ReadOnlyFilesystem)
        }
        fn rename(&self, _src: &str, _dst_dir: &DirNode, _dst: &str) -> VfsResult<()> {
            Err(VfsError::ReadOnlyFilesystem)
        }
    }

    fn mock_filesystem() -> Filesystem {
        Filesystem::new(Arc::new(MockFs))
    }

    fn make_dir_entry(name: &str) -> DirEntry {
        let node: Arc<dyn DirNodeOps> = Arc::new(MockNode);
        DirEntry::new_dir(
            |_| DirNode::new(node),
            Reference::new(None, name.to_string()),
        )
    }

    #[test]
    fn walk_tree_root_only() {
        let fs = mock_filesystem();
        let root = Mountpoint::new_root(&fs);
        let result = root.walk_tree();
        assert_eq!(result.len(), 1);
        let (mount_id, parent_id, mp) = &result[0];
        assert_eq!(*mount_id, root.mount_id());
        assert_eq!(*parent_id, root.mount_id());
        assert!(Arc::ptr_eq(mp, &root));
    }

    #[test]
    fn walk_tree_root_two_children_one_grandchild() {
        let fs = mock_filesystem();
        let root = Mountpoint::new_root(&fs);
        let root_id = root.mount_id();

        let child1_entry = make_dir_entry("child1");
        let child2_entry = make_dir_entry("child2");
        let grandchild_entry = make_dir_entry("grandchild");

        let child1 = Mountpoint::new_with_root(
            child1_entry.clone(),
            Some(root.root_location()),
            root.device() + 1,
        );
        let child2 = Mountpoint::new_with_root(
            child2_entry.clone(),
            Some(root.root_location()),
            root.device() + 2,
        );
        let grandchild = Mountpoint::new_with_root(
            grandchild_entry.clone(),
            Some(child1.root_location()),
            root.device() + 3,
        );

        root.children
            .lock()
            .insert(child1_entry.key(), child1.clone());
        root.children
            .lock()
            .insert(child2_entry.key(), child2.clone());
        child1
            .children
            .lock()
            .insert(grandchild_entry.key(), grandchild.clone());

        let result = root.walk_tree();
        assert_eq!(result.len(), 4);

        let child1_id = child1.mount_id();
        let child2_id = child2.mount_id();
        let grandchild_id = grandchild.mount_id();

        let ids: Vec<u64> = result.iter().map(|(id, ..)| *id).collect();
        for expected in [root_id, child1_id, child2_id, grandchild_id] {
            assert!(
                ids.contains(&expected),
                "missing mount_id {expected} in {ids:?}"
            );
        }

        for (mount_id, parent_id, _) in &result {
            let expected_parent = match *mount_id {
                id if id == root_id => root_id,
                id if id == child1_id || id == child2_id => root_id,
                id if id == grandchild_id => child1_id,
                _ => panic!("unexpected mount_id {mount_id}"),
            };
            assert_eq!(*parent_id, expected_parent, "mount_id {mount_id}");
        }
    }
}
