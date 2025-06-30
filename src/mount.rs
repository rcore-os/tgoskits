use core::{
    iter,
    sync::atomic::{AtomicU64, Ordering},
};

use alloc::{
    collections::btree_map::BTreeMap,
    string::String,
    sync::{Arc, Weak},
    vec,
};
use inherit_methods_macro::inherit_methods;
use lock_api::{Mutex, RawMutex};

use crate::{
    DirEntry, DirEntrySink, Filesystem, FilesystemOps, Metadata, MetadataUpdate, NodePermission,
    NodeType, OpenOptions, ReferenceKey, VfsError, VfsResult,
    path::{DOT, DOTDOT, PathBuf},
};

pub struct Mountpoint<M> {
    /// Root dir entry in the mountpoint.
    root: DirEntry<M>,
    /// Location in the parent mountpoint.
    location: Option<Location<M>>,
    /// Children of the mountpoint.
    children: Mutex<M, BTreeMap<ReferenceKey, Weak<Self>>>,
    /// Device ID
    device: u64,
}
impl<M: RawMutex> Mountpoint<M> {
    pub fn new(fs: &Filesystem<M>, location_in_parent: Option<Location<M>>) -> Arc<Self> {
        static DEVICE_COUNTER: AtomicU64 = AtomicU64::new(1);

        let root = fs.root_dir();
        Arc::new(Self {
            root,
            location: location_in_parent,
            children: Mutex::default(),
            device: DEVICE_COUNTER.fetch_add(1, Ordering::Relaxed),
        })
    }
    pub fn new_root(fs: &Filesystem<M>) -> Arc<Self> {
        Self::new(fs, None)
    }

    pub fn root_location(self: &Arc<Self>) -> Location<M> {
        Location::new(self.clone(), self.root.clone())
    }

    /// Returns the location in the parent mountpoint.
    pub fn location(&self) -> Option<Location<M>> {
        self.location.clone()
    }

    pub fn is_root(&self) -> bool {
        self.location.is_none()
    }

    /// Returns the effective mountpoint.
    ///
    /// For example, first `mount /dev/sda1 /mnt` and then `mount /dev/sda2
    /// /mnt`. After the second mount is completed, the content of the first
    /// mount will be overridden (root mount -> mnt1 -> mnt2). We need to
    /// return `mnt2` for `mnt1.effective_mountpoint()`.
    pub(crate) fn effective_mountpoint(self: &Arc<Self>) -> Arc<Mountpoint<M>> {
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

pub struct Location<M> {
    mountpoint: Arc<Mountpoint<M>>,
    entry: DirEntry<M>,
}
impl<M> Clone for Location<M> {
    fn clone(&self) -> Self {
        Self {
            mountpoint: self.mountpoint.clone(),
            entry: self.entry.clone(),
        }
    }
}

#[inherit_methods(from = "self.entry")]
impl<M: RawMutex> Location<M> {
    pub fn inode(&self) -> u64;
    pub fn filesystem(&self) -> &dyn FilesystemOps<M>;
    pub fn update_metadata(&self, update: MetadataUpdate) -> VfsResult<()>;
    pub fn len(&self) -> VfsResult<u64>;
    pub fn sync(&self, data_only: bool) -> VfsResult<()>;

    pub fn is_file(&self) -> bool;
    pub fn is_dir(&self) -> bool;

    pub fn node_type(&self) -> NodeType;
    pub fn is_root_of_mount(&self) -> bool;

    pub fn read_link(&self) -> VfsResult<String>;
}

impl<M: RawMutex> Location<M> {
    pub fn new(mountpoint: Arc<Mountpoint<M>>, entry: DirEntry<M>) -> Self {
        Self { mountpoint, entry }
    }

    fn wrap(&self, entry: DirEntry<M>) -> Self {
        Self::new(self.mountpoint.clone(), entry)
    }

    pub fn mountpoint(&self) -> &Arc<Mountpoint<M>> {
        &self.mountpoint
    }
    pub fn entry(&self) -> &DirEntry<M> {
        &self.entry
    }

    pub fn name(&self) -> &str {
        if self.is_root_of_mount() {
            self.mountpoint.location.as_ref().map_or("", Location::name)
        } else {
            self.entry.name()
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
        self.entry.as_dir().map_or(false, |it| it.is_mountpoint())
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
            return Err(VfsError::EXDEV);
        }
        self.entry
            .as_dir()?
            .link(name, &node.entry)
            .map(|entry| self.wrap(entry))
    }

    pub fn rename(&self, src_name: &str, dst_dir: &Self, dst_name: &str) -> VfsResult<()> {
        if !Arc::ptr_eq(&self.mountpoint, &dst_dir.mountpoint) {
            return Err(VfsError::EXDEV);
        }
        if !self.ptr_eq(&dst_dir) && self.entry.is_ancestor_of(&dst_dir.entry)? {
            return Err(VfsError::EINVAL);
        }
        self.entry
            .as_dir()?
            .rename(src_name, dst_dir.entry.as_dir()?, dst_name)
    }

    pub fn unlink(&self, name: &str, is_dir: bool) -> VfsResult<()> {
        self.entry.as_dir()?.unlink(name, is_dir)
    }

    pub fn open_file(&self, name: &str, options: &OpenOptions) -> VfsResult<Location<M>> {
        self.entry
            .as_dir()?
            .open_file(name, options)
            .map(|entry| self.wrap(entry).resolve_mountpoint())
    }

    pub fn read_dir(&self, offset: u64, sink: &mut dyn DirEntrySink) -> VfsResult<usize> {
        self.entry.as_dir()?.read_dir(offset, sink)
    }

    pub fn mount(&self, fs: &Filesystem<M>) -> VfsResult<Arc<Mountpoint<M>>> {
        let mut mountpoint = self.entry.as_dir()?.mountpoint.lock();
        if mountpoint.is_some() {
            return Err(VfsError::EBUSY);
        }
        let result = Mountpoint::new(&fs, Some(self.clone()));
        *mountpoint = Some(result.clone());
        self.mountpoint
            .children
            .lock()
            .insert(self.entry.key(), Arc::downgrade(&result));
        Ok(result)
    }

    pub fn unmount(&self) -> VfsResult<()> {
        if !self.is_root_of_mount() {
            return Err(VfsError::EINVAL);
        }
        let Some(parent_loc) = &self.mountpoint.location else {
            return Err(VfsError::EINVAL);
        };
        assert!(self.entry.ptr_eq(&self.mountpoint.root));
        self.entry.as_dir()?.forget();
        *parent_loc.entry.as_dir()?.mountpoint.lock() = None;
        Ok(())
    }
}
