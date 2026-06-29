use alloc::{
    borrow::{Cow, ToOwned},
    string::{String, ToString},
    sync::{Arc, Weak},
    vec,
    vec::Vec,
};
use core::{
    any::Any,
    fmt::Write,
    iter, mem,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    task::Context,
    time::Duration,
};

use hashbrown::HashMap;
use inherit_methods_macro::inherit_methods;

use crate::{
    DeviceId, DirEntry, DirEntrySink, DirNode, DirNodeOps, Filesystem, FilesystemOps, FsIoEvents,
    FsPollable, Metadata, MetadataUpdate, Mutex, MutexGuard, NodeFlags, NodeOps, NodePermission,
    NodeType, OpenOptions, Reference, ReferenceKey, TypeMap, VfsError, VfsResult, WeakDirEntry,
    path::{DOT, DOTDOT, PathBuf, verify_entry_name},
};

static DEVICE_COUNTER: AtomicU64 = AtomicU64::new(1);
static SYNTHETIC_MOUNT_INODE_COUNTER: AtomicU64 = AtomicU64::new(1_u64 << 63);
static PROPAGATION_GROUP_COUNTER: AtomicU64 = AtomicU64::new(1);

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

#[derive(Debug, Clone, Copy, Default)]
struct PropagationState {
    shared_group: Option<u64>,
    slave: bool,
    unbindable: bool,
}

#[derive(Debug, Clone)]
struct MountLocation {
    mountpoint: Weak<Mountpoint>,
    entry: DirEntry,
}

impl MountLocation {
    fn new(location: &Location) -> Self {
        Self {
            mountpoint: Arc::downgrade(location.mountpoint()),
            entry: location.entry.clone(),
        }
    }

    fn upgrade(&self) -> Option<Location> {
        Some(Location::new(
            self.mountpoint.upgrade()?,
            self.entry.clone(),
        ))
    }
}

#[derive(Debug)]
pub struct Mountpoint {
    /// Root dir entry in the mountpoint.
    root: DirEntry,
    /// Location in the parent mountpoint. `None` for a namespace root mount.
    location: Mutex<Option<MountLocation>>,
    /// Children of the mountpoint.
    children: Mutex<HashMap<ReferenceKey, Arc<Self>>>,
    /// Device ID
    device: u64,
    /// Read-only flag for this mountpoint.
    readonly: AtomicBool,
    /// Expire mark for umount2(MNT_EXPIRE).
    expired: AtomicBool,
    /// Mount propagation type.
    propagation: Mutex<PropagationState>,
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
            location: Mutex::new(location_in_parent.as_ref().map(MountLocation::new)),
            children: Mutex::new(HashMap::default()),
            device,
            readonly: AtomicBool::new(false),
            expired: AtomicBool::new(false),
            propagation: Mutex::new(PropagationState::default()),
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

    fn bind(
        source: &Location,
        location_in_parent: Location,
        recursive: bool,
    ) -> VfsResult<Arc<Self>> {
        fn clone_bound_subtree(source: &Arc<Mountpoint>, location: Location) -> Arc<Mountpoint> {
            let cloned =
                Mountpoint::new_with_root(source.root.clone(), Some(location), source.device);
            cloned
                .readonly
                .store(source.is_readonly(), Ordering::Release);

            let children = source.children.lock().values().cloned().collect::<Vec<_>>();
            for child in children {
                if child.is_unbindable() {
                    continue;
                }
                let Some(source_location) = child.location() else {
                    continue;
                };
                let cloned_location = Location::new(cloned.clone(), source_location.entry.clone());
                let cloned_child = clone_bound_subtree(&child, cloned_location);
                cloned
                    .children
                    .lock()
                    .insert(source_location.entry.key(), cloned_child);
            }
            cloned.inherit_propagation(source);
            cloned
        }

        let result = Self::new_with_root(
            source.entry.clone(),
            Some(location_in_parent),
            source.mountpoint.device(),
        );
        result
            .readonly
            .store(source.mountpoint.is_readonly(), Ordering::Release);
        if recursive {
            let children_to_bind = source
                .mountpoint
                .children
                .lock()
                .values()
                .cloned()
                .collect::<Vec<_>>();
            for child in children_to_bind {
                if child.is_unbindable() {
                    continue;
                }
                let Some(source_location) = child.location() else {
                    continue;
                };
                if !source.entry.ptr_eq(&source_location.entry)
                    && !source.entry.is_ancestor_of(&source_location.entry)?
                {
                    continue;
                }
                let cloned_location = Location::new(result.clone(), source_location.entry.clone());
                let cloned_child = clone_bound_subtree(&child, cloned_location);
                result
                    .children
                    .lock()
                    .insert(source_location.entry.key(), cloned_child);
            }
        }
        Ok(result)
    }

    pub fn root_location(self: &Arc<Self>) -> Location {
        Location::new(self.clone(), self.root.clone())
    }

    /// Clone this mount tree for a new mount namespace.
    ///
    /// Filesystems and dentries remain shared, while every mount node and all
    /// parent/child topology are copied.  This is the equivalent of Linux
    /// `copy_tree()` used by `copy_mnt_ns()`.
    pub fn clone_tree_and_remap(
        self: &Arc<Self>,
        locations: &[Location],
    ) -> VfsResult<(Arc<Self>, Vec<Location>)> {
        fn clone_subtree(
            source: &Arc<Mountpoint>,
            location: Option<Location>,
            mounts: &mut HashMap<usize, Arc<Mountpoint>>,
            pairs: &mut Vec<(Arc<Mountpoint>, Arc<Mountpoint>)>,
        ) -> Arc<Mountpoint> {
            let cloned = Mountpoint::new_with_root(source.root.clone(), location, source.device);
            cloned
                .readonly
                .store(source.is_readonly(), Ordering::Release);
            mounts.insert(Arc::as_ptr(source) as usize, cloned.clone());
            pairs.push((source.clone(), cloned.clone()));

            let children = source.children.lock().values().cloned().collect::<Vec<_>>();
            for child in children {
                let Some(source_location) = child.location() else {
                    continue;
                };
                let cloned_location = Location::new(cloned.clone(), source_location.entry.clone());
                let cloned_child = clone_subtree(&child, Some(cloned_location), mounts, pairs);
                cloned
                    .children
                    .lock()
                    .insert(source_location.entry.key(), cloned_child);
            }
            cloned
        }

        let mut mounts = HashMap::default();
        let mut pairs = Vec::new();
        let root = clone_subtree(self, None, &mut mounts, &mut pairs);
        for (source, cloned) in pairs {
            cloned.inherit_propagation(&source);
        }
        let remapped = locations
            .iter()
            .map(|location| {
                let key = Arc::as_ptr(location.mountpoint()) as usize;
                let mountpoint = mounts.get(&key).ok_or(VfsError::InvalidInput)?.clone();
                Ok(Location::new(mountpoint, location.entry.clone()))
            })
            .collect::<VfsResult<Vec<_>>>()?;
        Ok((root, remapped))
    }

    /// Render the mount tree using the Linux `/proc/[pid]/mountinfo` format.
    ///
    /// Mount IDs are generated for the snapshot and only need to remain
    /// internally consistent for the duration of this read.
    pub fn render_mountinfo(self: &Arc<Self>) -> VfsResult<String> {
        fn escape_path(path: &str) -> String {
            path.replace('\\', "\\134")
                .replace(' ', "\\040")
                .replace('\t', "\\011")
                .replace('\n', "\\012")
        }

        fn collect(
            mountpoint: &Arc<Mountpoint>,
            parent_id: u64,
            mount_id: u64,
            output: &mut String,
            next_id: &mut u64,
        ) -> VfsResult<()> {
            let mount_path = mountpoint.location().map_or_else(
                || Ok("/".to_owned()),
                |location| location.absolute_path().map(|path| path.to_string()),
            )?;
            let fs_type = mountpoint.root.filesystem().name();
            let options = if mountpoint.is_readonly() { "ro" } else { "rw" };
            let mut optional = String::new();
            if let Some(group) = mountpoint.shared_group() {
                write!(optional, " shared:{group}").map_err(|_| VfsError::InvalidInput)?;
            }
            if let Some(group) = mountpoint.master_group() {
                write!(optional, " master:{group}").map_err(|_| VfsError::InvalidInput)?;
            }
            writeln!(
                output,
                "{mount_id} {parent_id} 0:{} / {} {options},relatime{optional} - {fs_type} \
                 {fs_type} {options}",
                mountpoint.device(),
                escape_path(&mount_path),
            )
            .map_err(|_| VfsError::InvalidInput)?;

            let mut children = mountpoint
                .children
                .lock()
                .values()
                .cloned()
                .collect::<Vec<_>>();
            children.sort_by_cached_key(|child| {
                child
                    .location()
                    .and_then(|location| location.absolute_path().ok())
                    .unwrap_or_default()
            });
            for child in children {
                let child_id = *next_id;
                *next_id = next_id.saturating_add(1);
                collect(&child, mount_id, child_id, output, next_id)?;
            }
            Ok(())
        }

        let mut output = String::new();
        let mut next_id = 2;
        collect(self, 0, 1, &mut output, &mut next_id)?;
        Ok(output)
    }

    /// Returns the location in the parent mountpoint.
    pub fn location(&self) -> Option<Location> {
        self.location.lock().as_ref()?.upgrade()
    }

    pub fn is_root(&self) -> bool {
        self.location.lock().is_none()
    }

    /// Pivot the mount tree: the old root (`self`) is detached and re-attached
    /// at `put_old` under `new_root_mp`, which becomes the namespace root.
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
                let parent = old_loc.mountpoint.upgrade().ok_or(VfsError::InvalidInput)?;
                parent.children.lock().remove(&old_loc.entry.key());
            }
            // new_root becomes the namespace root.
            *new_root_loc = None;
        }

        // 2. Attach old root at put_old under new_root.
        {
            new_root_mp
                .children
                .lock()
                .insert(put_old.entry.key(), self.clone());
            *self.location.lock() = Some(MountLocation::new(put_old));
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
        loop {
            let next = mountpoint
                .children
                .lock()
                .get(&mountpoint.root.key())
                .cloned();
            let Some(next) = next else {
                return mountpoint;
            };
            mountpoint = next;
        }
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

    fn propagation(&self) -> PropagationState {
        *self.propagation.lock()
    }

    pub fn is_shared(&self) -> bool {
        self.propagation().shared_group.is_some()
    }

    pub fn is_slave(&self) -> bool {
        self.propagation().slave
    }

    pub fn is_unbindable(&self) -> bool {
        self.propagation().unbindable
    }

    pub fn shared_group(&self) -> Option<u64> {
        self.propagation().shared_group
    }

    pub fn master_group(&self) -> Option<u64> {
        self.masters
            .lock()
            .iter()
            .filter_map(Weak::upgrade)
            .find_map(|master| master.shared_group())
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
        let mut propagation = self.propagation.lock();
        propagation.unbindable = false;
        if propagation.shared_group.is_none() {
            propagation.shared_group =
                Some(PROPAGATION_GROUP_COUNTER.fetch_add(1, Ordering::Relaxed));
        }
    }

    pub fn set_private(self: &Arc<Self>) {
        self.remove_from_shared_group();
        self.remove_from_masters();
        *self.propagation.lock() = PropagationState::default();
    }

    pub fn set_slave(self: &Arc<Self>) {
        let mut masters = Vec::new();
        if self.is_shared() {
            masters.extend(self.peers.lock().iter().filter_map(Weak::upgrade));
        }

        self.remove_from_shared_group();
        self.remove_from_masters();
        let mut propagation = self.propagation.lock();
        propagation.shared_group = None;
        propagation.unbindable = false;
        propagation.slave = !masters.is_empty();
        drop(propagation);
        for master in masters {
            master.slaves.lock().push(Arc::downgrade(self));
            self.masters.lock().push(Arc::downgrade(&master));
        }
    }

    pub fn set_unbindable(self: &Arc<Self>) {
        self.set_private();
        self.propagation.lock().unbindable = true;
    }

    fn subtree(self: &Arc<Self>) -> Vec<Arc<Self>> {
        fn collect(mountpoint: &Arc<Mountpoint>, output: &mut Vec<Arc<Mountpoint>>) {
            output.push(mountpoint.clone());
            let children = mountpoint
                .children
                .lock()
                .values()
                .cloned()
                .collect::<Vec<_>>();
            for child in children {
                collect(&child, output);
            }
        }

        let mut output = Vec::new();
        collect(self, &mut output);
        output
    }

    pub fn set_shared_recursive(self: &Arc<Self>, recursive: bool) {
        if recursive {
            for mountpoint in self.subtree() {
                mountpoint.set_shared();
            }
        } else {
            self.set_shared();
        }
    }

    pub fn set_private_recursive(self: &Arc<Self>, recursive: bool) {
        if recursive {
            for mountpoint in self.subtree() {
                mountpoint.set_private();
            }
        } else {
            self.set_private();
        }
    }

    pub fn set_slave_recursive(self: &Arc<Self>, recursive: bool) {
        if recursive {
            for mountpoint in self.subtree() {
                mountpoint.set_slave();
            }
        } else {
            self.set_slave();
        }
    }

    pub fn set_unbindable_recursive(self: &Arc<Self>, recursive: bool) {
        if recursive {
            for mountpoint in self.subtree() {
                mountpoint.set_unbindable();
            }
        } else {
            self.set_unbindable();
        }
    }

    pub fn join_shared_group(self: &Arc<Self>, source: &Arc<Self>) {
        let mut group = vec![source.clone()];
        group.extend(source.peers.lock().iter().filter_map(Weak::upgrade));

        self.remove_from_shared_group();
        {
            let mut propagation = self.propagation.lock();
            propagation.unbindable = false;
            propagation.shared_group = source
                .shared_group()
                .or_else(|| Some(PROPAGATION_GROUP_COUNTER.fetch_add(1, Ordering::Relaxed)));
        }
        for member in group {
            if Arc::ptr_eq(&member, self) {
                continue;
            }
            member.peers.lock().push(Arc::downgrade(self));
            self.peers.lock().push(Arc::downgrade(&member));
        }
    }

    fn inherit_slave_relationship(self: &Arc<Self>, source: &Arc<Self>) {
        self.remove_from_masters();
        let masters = source
            .masters
            .lock()
            .iter()
            .filter_map(Weak::upgrade)
            .collect::<Vec<_>>();
        self.propagation.lock().slave = !masters.is_empty();
        for master in masters {
            master.slaves.lock().push(Arc::downgrade(self));
            self.masters.lock().push(Arc::downgrade(&master));
        }
    }

    fn inherit_propagation(self: &Arc<Self>, source: &Arc<Self>) {
        if source.is_unbindable() {
            self.set_unbindable();
            return;
        }
        if source.is_slave() {
            self.inherit_slave_relationship(source);
        }
        if source.is_shared() {
            self.join_shared_group(source);
        }
    }

    fn become_slave_of(self: &Arc<Self>, master: &Arc<Self>) {
        self.set_private();
        let mut masters = vec![master.clone()];
        masters.extend(master.peers.lock().iter().filter_map(Weak::upgrade));
        self.propagation.lock().slave = !masters.is_empty();
        for member in masters {
            member.slaves.lock().push(Arc::downgrade(self));
            self.masters.lock().push(Arc::downgrade(&member));
        }
    }

    fn propagation_targets(self: &Arc<Self>) -> Vec<(Arc<Self>, bool)> {
        fn add_slave_tree(mountpoint: &Arc<Mountpoint>, output: &mut Vec<(Arc<Mountpoint>, bool)>) {
            let slaves = mountpoint
                .slaves
                .lock()
                .iter()
                .filter_map(Weak::upgrade)
                .collect::<Vec<_>>();
            for slave in slaves {
                if output.iter().any(|(item, _)| Arc::ptr_eq(item, &slave)) {
                    continue;
                }
                output.push((slave.clone(), true));
                add_slave_tree(&slave, output);
                let peers = slave
                    .peers
                    .lock()
                    .iter()
                    .filter_map(Weak::upgrade)
                    .collect::<Vec<_>>();
                for peer in peers {
                    if !output.iter().any(|(item, _)| Arc::ptr_eq(item, &peer)) {
                        output.push((peer.clone(), true));
                        add_slave_tree(&peer, output);
                    }
                }
            }
        }

        let peers = self
            .peers
            .lock()
            .iter()
            .filter_map(Weak::upgrade)
            .collect::<Vec<_>>();
        let mut output = peers
            .iter()
            .cloned()
            .map(|peer| (peer, false))
            .collect::<Vec<_>>();
        add_slave_tree(self, &mut output);
        for peer in peers {
            add_slave_tree(&peer, &mut output);
        }
        output
    }

    fn clone_propagated_subtree(
        source: &Arc<Self>,
        location: Location,
        as_slave: bool,
    ) -> Arc<Self> {
        let cloned = Self::new_with_root(source.root.clone(), Some(location), source.device);
        cloned
            .readonly
            .store(source.is_readonly(), Ordering::Release);

        let children = source.children.lock().values().cloned().collect::<Vec<_>>();
        for child in children {
            let Some(source_location) = child.location() else {
                continue;
            };
            let cloned_location = Location::new(cloned.clone(), source_location.entry.clone());
            let cloned_child = Self::clone_propagated_subtree(&child, cloned_location, as_slave);
            cloned
                .children
                .lock()
                .insert(source_location.entry.key(), cloned_child);
        }

        if as_slave && source.is_shared() {
            cloned.become_slave_of(source);
        } else if source.is_shared() {
            cloned.join_shared_group(source);
        } else {
            cloned.inherit_propagation(source);
        }
        cloned
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
        if !child.is_shared() {
            child.set_shared();
        }
        let mut path_components = vec![];
        let mut current = source_location.clone();
        while !current.is_root_of_mount() {
            path_components.push(current.name().into_owned());
            current = current.parent().ok_or(VfsError::InvalidInput)?;
        }
        path_components.reverse();

        for (target_parent, as_slave) in source_parent.propagation_targets() {
            let mut location = target_parent.root_location();
            for component in &path_components {
                location = location.lookup_no_follow(component)?;
            }
            if location.is_mountpoint() {
                return Err(VfsError::ResourceBusy);
            }
            let replicated = Self::clone_propagated_subtree(child, location.clone(), as_slave);
            Self::attach_child(&target_parent, location, &replicated)?;
        }
        Ok(())
    }

    fn propagate_unmount(source_parent: &Arc<Self>, source_location: &Location) {
        let key = source_location.entry.key();
        for (target_parent, _) in source_parent.propagation_targets() {
            if let Some(propagated) = target_parent.children.lock().remove(&key) {
                *propagated.location.lock() = None;
            }
        }
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

        let Some(old_location) = self.location() else {
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

        *self.location.lock() = Some(MountLocation::new(new_location));
        Ok(())
    }

    pub fn detach(self: &Arc<Self>) -> VfsResult<()> {
        if self.is_root() {
            return Err(VfsError::InvalidInput);
        }
        let Some(location) = self.location() else {
            return Err(VfsError::InvalidInput);
        };
        if location.mountpoint.is_shared() {
            Self::propagate_unmount(location.mountpoint(), &location);
        }
        location
            .mountpoint
            .children
            .lock()
            .remove(&location.entry.key());
        *self.location.lock() = None;
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

    pub fn is_root_of_mount(&self) -> bool {
        self.entry.ptr_eq(&self.mountpoint.root)
    }

    pub fn namespace_root_mountpoint(&self) -> Arc<Mountpoint> {
        let mut root = self.mountpoint.clone();
        while let Some(parent) = root
            .location()
            .map(|location| location.mountpoint().clone())
        {
            root = parent;
        }
        root
    }

    /// Render the mount tree containing this location.
    pub fn render_mountinfo(&self) -> VfsResult<String> {
        self.namespace_root_mountpoint().render_mountinfo()
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
                .location()
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
            let mut entry = cur.entry.clone();
            while !entry.ptr_eq(&cur.mountpoint.root) {
                if !entry.name().is_empty() {
                    components.push(entry.name().to_owned());
                }
                entry = entry.parent().ok_or(VfsError::InvalidInput)?;
            }
            let Some(location) = cur.mountpoint.location() else {
                break;
            };
            cur = location;
        }
        Ok(iter::once("/")
            .chain(components.iter().map(String::as_str).rev())
            .collect())
    }

    pub fn ptr_eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.mountpoint, &other.mountpoint) && self.entry.ptr_eq(&other.entry)
    }

    pub fn is_mountpoint(&self) -> bool {
        self.mountpoint
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
        if self
            .mountpoint
            .children
            .lock()
            .contains_key(&self.entry.key())
        {
            return Err(VfsError::ResourceBusy);
        }
        self.mountpoint
            .children
            .lock()
            .insert(self.entry.key(), result.clone());
        if should_propagate {
            Mountpoint::propagate_new_child(self.mountpoint(), self, &result)?;
        }
        Ok(result)
    }

    pub fn bind_mount(&self, source: &Self, recursive: bool) -> VfsResult<Arc<Mountpoint>> {
        let source_mountpoint = source.mountpoint().clone();
        if source_mountpoint.is_unbindable() {
            return Err(VfsError::InvalidInput);
        }

        self.check_is_dir()?;
        let result = Mountpoint::bind(source, self.clone(), recursive)?;
        result.inherit_propagation(&source_mountpoint);
        if self
            .mountpoint
            .children
            .lock()
            .contains_key(&self.entry.key())
        {
            result.set_private();
            return Err(VfsError::ResourceBusy);
        }
        self.mountpoint
            .children
            .lock()
            .insert(self.entry.key(), result.clone());
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
        if let Some(parent_loc) = self.mountpoint.location() {
            if parent_loc.mountpoint.is_shared() {
                Mountpoint::propagate_unmount(parent_loc.mountpoint(), &parent_loc);
            }
            parent_loc
                .mountpoint
                .children
                .lock()
                .remove(&parent_loc.entry.key());
        }
        *self.mountpoint.location.lock() = None;
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
