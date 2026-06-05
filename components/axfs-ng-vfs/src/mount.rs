use alloc::{
    borrow::Cow,
    string::String,
    sync::{Arc, Weak},
    vec,
    vec::Vec,
};
use core::{
    iter, mem,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    task::Context,
};

use axpoll::{IoEvents, Pollable};
use hashbrown::HashMap;
use inherit_methods_macro::inherit_methods;
use log::warn;

use crate::{
    DirEntry, DirEntrySink, Filesystem, FilesystemOps, Metadata, MetadataUpdate, Mutex, MutexGuard,
    NodeFlags, NodePermission, NodeType, OpenOptions, ReferenceKey, TypeMap, VfsError, VfsResult,
    path::{DOT, DOTDOT, PathBuf},
};

static DEVICE_COUNTER: AtomicU64 = AtomicU64::new(1);

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
    /// Children of the mountpoint.
    children: Mutex<HashMap<ReferenceKey, Weak<Self>>>,
    /// Device ID
    device: u64,
    /// Read-only flag for this mountpoint.
    readonly: AtomicBool,
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
            readonly: AtomicBool::new(false),
            expired: AtomicBool::new(false),
            propagation: Mutex::new(PropagationType::Private),
            peers: Mutex::default(),
            slaves: Mutex::default(),
            masters: Mutex::default(),
        })
    }

    pub fn new(fs: &Filesystem, location_in_parent: Option<Location>) -> Arc<Self> {
        Self::new_with_root(
            fs.root_dir(),
            location_in_parent,
            DEVICE_COUNTER.fetch_add(1, Ordering::Relaxed),
        )
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
            let mut children_to_bind: Vec<_> = source
                .mountpoint
                .children
                .lock()
                .iter()
                .map(|(key, child)| (key.clone(), child.clone()))
                .collect();
            children_to_bind
                .retain(|(_, child)| child.upgrade().is_none_or(|child| !child.is_unbindable()));
            let mut result_children = result.children.lock();
            for (key, child) in children_to_bind {
                result_children.insert(key, child);
            }
        }
        result
    }

    pub fn root_location(self: &Arc<Self>) -> Location {
        Location::new(self.clone(), self.root.clone())
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
                *old_loc.entry.as_dir()?.mountpoint.lock() = None;
            }
            // new_root becomes the global root.
            *new_root_loc = None;
        }

        // 2. Attach old root at put_old under new_root.
        {
            *put_old.entry.as_dir()?.mountpoint.lock() = Some(self.clone());
            new_root_mp
                .children
                .lock()
                .insert(put_old.entry.key(), Arc::downgrade(self));
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
        while let Some(mount) = mountpoint.root.as_dir().unwrap().mountpoint() {
            mountpoint = mount;
        }
        mountpoint
    }

    pub fn device(self: &Arc<Self>) -> u64 {
        self.device
    }

    pub fn is_readonly(&self) -> bool {
        self.readonly.load(Ordering::Acquire)
    }

    pub fn set_readonly(&self, readonly: bool) {
        self.readonly.store(readonly, Ordering::Release);
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
    }

    pub fn set_private(self: &Arc<Self>) {
        self.remove_from_shared_group();
        self.remove_from_masters();
        *self.propagation.lock() = PropagationType::Private;
    }

    pub fn set_slave(self: &Arc<Self>) {
        let mut masters = Vec::new();
        if self.is_shared() {
            masters.extend(self.peers.lock().iter().filter_map(Weak::upgrade));
        }

        self.remove_from_shared_group();
        self.remove_from_masters();
        *self.propagation.lock() = PropagationType::Slave;
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
        for member in group {
            if Arc::ptr_eq(&member, self) {
                continue;
            }
            member.peers.lock().push(Arc::downgrade(self));
            self.peers.lock().push(Arc::downgrade(&member));
        }
    }

    fn attach_child(parent: &Arc<Self>, location: Location, child: &Arc<Self>) -> VfsResult<()> {
        *location.entry.as_dir()?.mountpoint.lock() = Some(child.clone());
        parent
            .children
            .lock()
            .insert(location.entry.key(), Arc::downgrade(child));
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

        *old_location.entry.as_dir()?.mountpoint.lock() = None;
        old_location
            .mountpoint
            .children
            .lock()
            .remove(&old_location.entry.key());

        *new_location.entry.as_dir()?.mountpoint.lock() = Some(self.clone());
        new_location
            .mountpoint
            .children
            .lock()
            .insert(new_location.entry.key(), Arc::downgrade(self));

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
        *location.entry.as_dir()?.mountpoint.lock() = None;
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

    pub fn is_root_of_mount(&self) -> bool;

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
        self.mountpoint.is_root() && self.entry.is_root_of_mount()
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
        self.entry.as_dir().is_ok_and(|it| it.is_mountpoint())
    }

    /// See [`Mountpoint::effective_mountpoint`].
    fn resolve_mountpoint(self) -> Self {
        let Some(mountpoint) = self
            .mountpoint
            .children
            .lock()
            .get(&self.entry.key())
            .and_then(Weak::upgrade)
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
        {
            let mut mountpoint = self.entry.as_dir()?.mountpoint.lock();
            if mountpoint.is_some() {
                return Err(VfsError::ResourceBusy);
            }
            *mountpoint = Some(result.clone());
        }
        self.mountpoint
            .children
            .lock()
            .insert(self.entry.key(), Arc::downgrade(&result));
        if should_propagate {
            Mountpoint::propagate_new_child(self.mountpoint(), self, &result)?;
        }
        Ok(result)
    }

    pub fn bind_mount(&self, source: &Self, recursive: bool) -> VfsResult<Arc<Mountpoint>> {
        if source.mountpoint().is_unbindable() {
            return Err(VfsError::InvalidInput);
        }

        let mut mountpoint = self.entry.as_dir()?.mountpoint.lock();
        if mountpoint.is_some() {
            return Err(VfsError::ResourceBusy);
        }
        let result = Mountpoint::bind(source, self.clone(), recursive);
        if source.mountpoint().is_shared() {
            result.join_shared_group(source.mountpoint());
        } else if source.mountpoint().is_slave() {
            result.set_slave();
        }
        *mountpoint = Some(result.clone());
        self.mountpoint
            .children
            .lock()
            .insert(self.entry.key(), Arc::downgrade(&result));
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
            *parent_loc.entry.as_dir()?.mountpoint.lock() = None;
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
            if let Some(child) = child.upgrade() {
                child.root_location().unmount_all()?;
            }
        }
        self.unmount()
    }
}

#[inherit_methods(from = "self.entry")]
impl Pollable for Location {
    fn poll(&self) -> IoEvents;

    fn register(&self, context: &mut Context<'_>, events: IoEvents);
}
