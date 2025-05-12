use alloc::{collections::btree_map::BTreeMap, sync::Arc, vec};
use inherit_methods_macro::inherit_methods;
use lock_api::{Mutex, RawMutex};

use crate::{
    path::{PathBuf, DOT, DOTDOT}, DirEntry, DirEntrySink, Filesystem, Metadata, NodePermission, NodeType, ReferenceKey, VfsError, VfsResult
};

pub struct Mountpoint<M> {
    /// Root dir entry in the mountpoint.
    root: DirEntry<M>,
    /// Location in the parent mountpoint.
    location: Option<Location<M>>,
    /// Children of the mountpoint.
    children: Mutex<M, BTreeMap<ReferenceKey, Arc<Self>>>,
}
impl<M: RawMutex> Mountpoint<M> {
    pub fn new_root(fs: &Filesystem<M>) -> Arc<Self> {
        let root = fs.root_dir();
        Arc::new(Self {
            root,
            location: None,
            children: Mutex::default(),
        })
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
        while let Some(mount) = mountpoint.root.mountpoint() {
            mountpoint = mount;
        }
        mountpoint
    }

    pub fn device(self: &Arc<Self>) -> u64 {
        Arc::as_ptr(self) as u64
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
    pub fn len(&self) -> VfsResult<u64>;
    pub fn sync(&self, data_only: bool) -> VfsResult<()>;

    pub fn is_file(&self) -> bool;
    pub fn is_dir(&self) -> bool;

    pub fn node_type(&self) -> NodeType;
    pub fn is_root_of_mount(&self) -> bool;
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
            self.mountpoint.root.name()
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
        Ok(components.iter().rev().collect())
    }

    pub fn ptr_eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.mountpoint, &other.mountpoint) && self.entry.ptr_eq(&other.entry)
    }

    /// See [`Mountpoint::effective_mountpoint`].
    fn resolve_mountpoint(self) -> Self {
        if !self.entry.is_root_of_mount() {
            return self;
        }

        let mountpoint = self.mountpoint.effective_mountpoint();
        let entry = mountpoint.root.clone();
        Self::new(mountpoint, entry)
    }

    pub fn lookup(&self, name: &str) -> VfsResult<Self> {
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

    pub fn open_file_or_create(
        &self,
        name: &str,
        create: bool,
        create_new: bool,
        permission: NodePermission,
    ) -> VfsResult<Location<M>> {
        self.entry
            .as_dir()?
            .open_file_or_create(name, create, create_new, permission)
            .map(|entry| self.wrap(entry))
    }

    pub fn read_dir(&self, offset: u64, sink: &mut dyn DirEntrySink) -> VfsResult<usize> {
        self.entry.as_dir()?.read_dir(offset, sink)
    }
}
