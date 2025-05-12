use core::ops::Deref;

use alloc::{borrow::ToOwned, collections::btree_map::BTreeMap, string::String, sync::Arc};
use lock_api::{Mutex, MutexGuard, RawMutex};

use crate::{NodeOps, NodePermission, NodeType, VfsError, VfsResult, path::verify_entry_name};

use super::DirEntry;

/// A trait for a sink that can receive directory entries.
pub trait DirEntrySink<M> {
    /// Accept a directory entry, returns `false` if the sink is full.
    ///
    /// `offset` is the offset of the next entry to be read.
    ///
    /// It's not recommended to operate on the node inside the `accept`
    /// function, since some filesystem may impose a lock while iterating the
    /// directory, and operating on the node may cause deadlock.
    fn accept(&mut self, entry: DirEntry<M>, offset: u64) -> bool;
}
impl<F: FnMut(DirEntry<M>, u64) -> bool, M> DirEntrySink<M> for F {
    fn accept(&mut self, entry: DirEntry<M>, offset: u64) -> bool {
        self(entry, offset)
    }
}

type DirChildren<M> = BTreeMap<String, DirEntry<M>>;

pub struct DirEntryVisitor<'a, M: RawMutex> {
    sink: &'a mut dyn DirEntrySink<M>,
    children: MutexGuard<'a, M, DirChildren<M>>,
}
impl<'a, M: RawMutex> DirEntryVisitor<'a, M> {
    pub fn accept_with(
        &mut self,
        name: String,
        offset: u64,
        f: impl FnOnce(&String) -> DirEntry<M>,
    ) -> bool {
        self.sink.accept(
            self.children.entry(name).or_insert_with_key(f).clone(),
            offset,
        )
    }
}

pub trait DirNodeOps<M: RawMutex>: NodeOps<M> {
    /// Reads directory entries.
    ///
    /// Returns the number of entries read.
    fn read_dir(&self, offset: u64, visitor: DirEntryVisitor<'_, M>) -> VfsResult<usize>;

    /// Lookups a directory entry by name.
    fn lookup(&self, name: &str) -> VfsResult<DirEntry<M>>;

    /// Creates a directory entry.
    fn create(
        &self,
        name: &str,
        node_type: NodeType,
        permission: NodePermission,
    ) -> VfsResult<DirEntry<M>>;

    /// Creates a link to a node.
    fn link(&self, name: &str, node: &DirEntry<M>) -> VfsResult<DirEntry<M>>;

    /// Unlinks a directory entry by name.
    ///
    /// If the entry is a non-empty directory, it should return `ENOTEMPTY` error.
    fn unlink(&self, name: &str) -> VfsResult<()>;

    /// Renames a directory entry, replacing the original entry (dst) if it
    /// already exists.
    ///
    /// If src and dst link to the same file, this should do nothing and return
    /// `Ok(())`.
    ///
    /// The caller should ensure:
    /// - If `src` is a directory, `dst` must not exist or be an empty
    ///   directory.
    /// - If `src` is not a directory, `dst` must not exist or not be a
    ///   directory.
    fn rename(&self, src_name: &str, dst_dir: &DirNode<M>, dst_name: &str) -> VfsResult<()>;
}

pub struct DirNode<M> {
    ops: Arc<dyn DirNodeOps<M>>,
    cache: Mutex<M, BTreeMap<String, DirEntry<M>>>,
}
impl<M> Deref for DirNode<M> {
    type Target = dyn NodeOps<M>;

    fn deref(&self) -> &Self::Target {
        &*self.ops
    }
}
impl<M> From<DirNode<M>> for Arc<dyn NodeOps<M>> {
    fn from(node: DirNode<M>) -> Self {
        node.ops.clone()
    }
}

impl<M: RawMutex> DirNode<M> {
    pub fn new(ops: Arc<dyn DirNodeOps<M>>) -> Self {
        Self {
            ops,
            cache: Mutex::new(BTreeMap::new()),
        }
    }

    pub fn inner(&self) -> &Arc<dyn DirNodeOps<M>> {
        &self.ops
    }

    pub fn downcast<T: Send + Sync + 'static>(&self) -> VfsResult<Arc<T>> {
        self.ops
            .clone()
            .into_any()
            .downcast()
            .map_err(|_| VfsError::EINVAL)
    }

    fn lookup_locked(&self, name: &str, children: &mut DirChildren<M>) -> VfsResult<DirEntry<M>> {
        use alloc::collections::btree_map::Entry;
        match children.entry(name.to_owned()) {
            Entry::Occupied(e) => Ok(e.get().clone()),
            Entry::Vacant(e) => {
                let node = self.ops.lookup(name)?;
                e.insert(node.clone());
                Ok(node)
            }
        }
    }

    /// Looks up a directory entry by name.
    pub fn lookup(&self, name: &str) -> VfsResult<DirEntry<M>> {
        self.lookup_locked(name, &mut self.cache.lock())
    }

    /// Reads directory entries.
    pub fn read_dir(&self, offset: u64, sink: &mut dyn DirEntrySink<M>) -> VfsResult<usize> {
        let children = self.cache.lock();
        let visitor = DirEntryVisitor { sink, children };
        self.ops.read_dir(offset, visitor)
    }

    /// Creates a link to a node.
    pub fn link(&self, name: &str, node: &DirEntry<M>) -> VfsResult<DirEntry<M>> {
        verify_entry_name(name)?;

        self.ops.link(name, node).inspect(|entry| {
            self.cache.lock().insert(name.to_owned(), entry.clone());
        })
    }

    /// Unlinks a directory entry by name.
    pub fn unlink(&self, name: &str, is_dir: bool) -> VfsResult<()> {
        verify_entry_name(name)?;

        let mut children = self.cache.lock();
        let entry = self.lookup_locked(name, &mut children)?;
        match (entry.is_dir(), is_dir) {
            (true, false) => return Err(VfsError::EISDIR),
            (false, true) => return Err(VfsError::ENOTDIR),
            _ => {}
        }

        self.ops.unlink(name).inspect(|_| {
            self.cache.lock().remove(name);
        })
    }

    /// Returns whether the directory contains children.
    pub fn has_children(&self) -> VfsResult<bool> {
        let mut has_children = false;
        self.read_dir(0, &mut |entry: DirEntry<M>, _offset| {
            if entry.name() != "." && entry.name() != ".." {
                has_children = true;
                false
            } else {
                true
            }
        })?;
        Ok(has_children)
    }

    /// Creates a directory entry.
    pub fn create(
        &self,
        name: &str,
        node_type: NodeType,
        permission: NodePermission,
    ) -> VfsResult<DirEntry<M>> {
        verify_entry_name(name)?;

        let entry = self.ops.create(name, node_type, permission)?;
        self.cache.lock().insert(name.to_owned(), entry.clone());
        Ok(entry)
    }

    /// Renames a directory entry.
    pub fn rename(&self, src_name: &str, dst_dir: &Self, dst_name: &str) -> VfsResult<()> {
        verify_entry_name(src_name)?;
        verify_entry_name(dst_name)?;

        let src = self.lookup(src_name)?;
        if src.node_type() == NodeType::Directory {
            if let Ok(dst) = dst_dir.lookup(dst_name) {
                if let Ok(dir) = dst.as_dir() {
                    if dir.has_children()? {
                        // God this chain is horrible
                        return Err(VfsError::ENOTEMPTY);
                    }
                }
            }
        } else if let Ok(dst) = dst_dir.lookup(dst_name) {
            if dst.node_type() == NodeType::Directory {
                return Err(VfsError::EISDIR);
            }
        }

        self.ops.rename(src_name, dst_dir, dst_name).inspect(|_| {
            self.cache.lock().remove(src_name);
        })
    }

    /// Opens a file in the directory, optionally creating it if it doesn't
    /// exist.
    pub fn open_file_or_create(
        &self,
        name: &str,
        create: bool,
        create_new: bool,
        permission: NodePermission,
    ) -> VfsResult<DirEntry<M>> {
        verify_entry_name(name)?;

        let mut children = self.cache.lock();
        match self.lookup_locked(name, &mut children) {
            Ok(val) => {
                if create_new {
                    return Err(VfsError::EEXIST);
                }
                return Ok(val);
            }
            Err(err) if err == VfsError::ENOENT && create => {}
            Err(err) => return Err(err),
        }

        self.create(name, NodeType::RegularFile, permission)
    }
}
