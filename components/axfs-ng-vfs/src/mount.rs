use alloc::{
    borrow::Cow,
    string::String,
    sync::{Arc, Weak},
    vec,
};
use core::{
    iter, mem,
    sync::atomic::{AtomicU64, Ordering},
    task::Context,
};

use axpoll::{IoEvents, Pollable};
use hashbrown::HashMap;
use inherit_methods_macro::inherit_methods;

use crate::{
    DirEntry, DirEntrySink, Filesystem, FilesystemOps, Metadata, MetadataUpdate, Mutex, MutexGuard,
    NodeFlags, NodePermission, NodeType, OpenOptions, ReferenceKey, TypeMap, VfsError, VfsResult,
    path::{DOT, DOTDOT, PathBuf},
};

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
}

impl Mountpoint {
    pub fn new(fs: &Filesystem, location_in_parent: Option<Location>) -> Arc<Self> {
        static DEVICE_COUNTER: AtomicU64 = AtomicU64::new(1);

        let root = fs.root_dir();
        Arc::new(Self {
            root,
            location: Mutex::new(location_in_parent),
            children: Mutex::default(),
            device: DEVICE_COUNTER.fetch_add(1, Ordering::Relaxed),
        })
    }

    pub fn new_root(fs: &Filesystem) -> Arc<Self> {
        Self::new(fs, None)
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

    pub fn update_metadata(&self, update: MetadataUpdate) -> VfsResult<()>;

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

    pub fn entry(&self) -> &DirEntry {
        &self.entry
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
        let Some(mountpoint) = self.entry.as_dir().ok().and_then(|it| it.mountpoint()) else {
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
    ) -> VfsResult<Self> {
        self.entry
            .as_dir()?
            .create(name, node_type, permission)
            .map(|entry| self.wrap(entry))
    }

    pub fn link(&self, name: &str, node: &Self) -> VfsResult<Self> {
        if !Arc::ptr_eq(&self.mountpoint, &node.mountpoint) {
            return Err(VfsError::CrossesDevices);
        }
        self.entry
            .as_dir()?
            .link(name, &node.entry)
            .map(|entry| self.wrap(entry))
    }

    pub fn rename(&self, src_name: &str, dst_dir: &Self, dst_name: &str) -> VfsResult<()> {
        if !Arc::ptr_eq(&self.mountpoint, &dst_dir.mountpoint) {
            return Err(VfsError::CrossesDevices);
        }
        if !self.ptr_eq(dst_dir) && self.entry.is_ancestor_of(&dst_dir.entry)? {
            return Err(VfsError::InvalidInput);
        }
        self.entry
            .as_dir()?
            .rename(src_name, dst_dir.entry.as_dir()?, dst_name)
    }

    pub fn unlink(&self, name: &str, is_dir: bool) -> VfsResult<()> {
        self.entry.as_dir()?.unlink(name, is_dir)
    }

    pub fn open_file(&self, name: &str, options: &OpenOptions) -> VfsResult<Location> {
        self.entry
            .as_dir()?
            .open_file(name, options)
            .map(|entry| self.wrap(entry).resolve_mountpoint())
    }

    pub fn read_dir(&self, offset: u64, sink: &mut dyn DirEntrySink) -> VfsResult<usize> {
        self.entry.as_dir()?.read_dir(offset, sink)
    }

    pub fn mount(&self, fs: &Filesystem) -> VfsResult<Arc<Mountpoint>> {
        let mut mountpoint = self.entry.as_dir()?.mountpoint.lock();
        if mountpoint.is_some() {
            return Err(VfsError::ResourceBusy);
        }
        let result = Mountpoint::new(fs, Some(self.clone()));
        *mountpoint = Some(result.clone());
        self.mountpoint
            .children
            .lock()
            .insert(self.entry.key(), Arc::downgrade(&result));
        Ok(result)
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

        self.entry.as_dir()?.forget();
        if let Some(parent_loc) = self.mountpoint.location.lock().as_ref() {
            *parent_loc.entry.as_dir()?.mountpoint.lock() = None;
        }
        Ok(())
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
