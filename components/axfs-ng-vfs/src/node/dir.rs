use alloc::{borrow::ToOwned, string::String, sync::Arc};
use core::{
    mem,
    ops::{Deref, DerefMut},
};

use hashbrown::HashMap;

use super::DirEntry;
use crate::{
    Mountpoint, Mutex, MutexGuard, NodeOps, NodePermission, NodeType, VfsError, VfsResult,
    path::{DOT, DOTDOT, MAX_NAME_LEN, verify_entry_name},
};

/// A trait for a sink that can receive directory entries.
pub trait DirEntrySink {
    /// Accept a directory entry, returns `false` if the sink is full.
    ///
    /// `offset` is the offset of the next entry to be read.
    ///
    /// It's not recommended to operate on the node inside the `accept`
    /// function, since some filesystem may impose a lock while iterating the
    /// directory, and operating on the node may cause deadlock.
    fn accept(&mut self, name: &str, ino: u64, node_type: NodeType, offset: u64) -> bool;
}

impl<F: FnMut(&str, u64, NodeType, u64) -> bool> DirEntrySink for F {
    fn accept(&mut self, name: &str, ino: u64, node_type: NodeType, offset: u64) -> bool {
        self(name, ino, node_type, offset)
    }
}

type DirChildren = HashMap<String, DirEntry>;
const DIR_CACHE_NESTED_LOCK_SUBCLASS: u32 = 1;

#[inline(always)]
fn lock_dir_cache(cache: &Mutex<DirChildren>, subclass: u32) -> MutexGuard<'_, DirChildren> {
    cache.lock_nested(subclass)
}

enum LockedDirCaches<'a> {
    Same {
        children: MutexGuard<'a, DirChildren>,
    },
    SrcThenDst {
        // Struct fields are dropped in declaration order. Keep the later
        // acquired lock first so lockdep sees LIFO release.
        dst: MutexGuard<'a, DirChildren>,
        src: MutexGuard<'a, DirChildren>,
    },
    DstThenSrc {
        src: MutexGuard<'a, DirChildren>,
        dst: MutexGuard<'a, DirChildren>,
    },
}

impl LockedDirCaches<'_> {
    fn src_mut(&mut self) -> &mut DirChildren {
        match self {
            Self::Same { children } => children.deref_mut(),
            Self::SrcThenDst { src, .. } | Self::DstThenSrc { src, .. } => src.deref_mut(),
        }
    }

    fn dst_mut(&mut self) -> &mut DirChildren {
        match self {
            Self::Same { children } => children.deref_mut(),
            Self::SrcThenDst { dst, .. } | Self::DstThenSrc { dst, .. } => dst.deref_mut(),
        }
    }
}
pub trait DirNodeOps: NodeOps {
    /// Reads directory entries.
    ///
    /// Returns the number of entries read.
    ///
    /// Implementations should ensure that `.` and `..` are present in the
    /// result.
    fn read_dir(&self, offset: u64, sink: &mut dyn DirEntrySink) -> VfsResult<usize>;

    /// Lookups a directory entry by name.
    fn lookup(&self, name: &str) -> VfsResult<DirEntry>;

    /// Returns whether directory entries can be cached.
    ///
    /// Some filesystems (like '/proc') may not support caching directory
    /// entries, as they may change frequently or not be backed by persistent
    /// storage.
    ///
    /// If this returns `false`, the directory will not be cached in dentry and
    /// each call to [`DirNode::lookup`] will end up calling [`lookup`].
    /// Implementations should take care to handle cases where [`lookup`] is
    /// called multiple times for the same name.
    fn is_cacheable(&self) -> bool {
        true
    }

    /// Returns whether this directory has child entries relevant to rmdir.
    fn has_children(&self) -> VfsResult<bool> {
        let mut has_children = false;
        self.read_dir(0, &mut |name: &str, _, _, _| {
            if name != DOT && name != DOTDOT {
                has_children = true;
                false
            } else {
                true
            }
        })?;
        Ok(has_children)
    }

    /// Creates a directory entry.
    fn create(
        &self,
        name: &str,
        node_type: NodeType,
        permission: NodePermission,
        uid: u32,
        gid: u32,
    ) -> VfsResult<DirEntry>;

    /// Creates a link to a node.
    fn link(&self, name: &str, node: &DirEntry) -> VfsResult<DirEntry>;

    /// Unlinks a directory entry by name.
    ///
    /// If the entry is a non-empty directory, it should return `ENOTEMPTY`
    /// error.
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
    fn rename(&self, src_name: &str, dst_dir: &DirNode, dst_name: &str) -> VfsResult<()>;
}

/// Options for opening (or creating) a directory entry.
///
/// See [`DirNode::open_file`] for more details.
#[derive(Debug, Clone)]
pub struct OpenOptions {
    pub create: bool,
    pub create_new: bool,
    pub node_type: NodeType,
    pub permission: NodePermission,
    pub user: Option<(u32, u32)>, // (uid, gid)
}

impl Default for OpenOptions {
    fn default() -> Self {
        Self {
            create: false,
            create_new: false,
            node_type: NodeType::RegularFile,
            permission: NodePermission::default(),
            user: None,
        }
    }
}

pub struct DirNode {
    ops: Arc<dyn DirNodeOps>,
    cache: Mutex<DirChildren>,
    pub(crate) mountpoint: Mutex<Option<Arc<Mountpoint>>>,
}

impl Deref for DirNode {
    type Target = dyn NodeOps;

    fn deref(&self) -> &Self::Target {
        &*self.ops
    }
}

impl From<DirNode> for Arc<dyn NodeOps> {
    fn from(node: DirNode) -> Self {
        node.ops.clone()
    }
}

impl DirNode {
    pub fn new(ops: Arc<dyn DirNodeOps>) -> Self {
        Self {
            ops,
            cache: Mutex::new(DirChildren::default()),
            mountpoint: Mutex::new(None),
        }
    }

    pub fn inner(&self) -> &Arc<dyn DirNodeOps> {
        &self.ops
    }

    pub fn downcast<T: DirNodeOps>(&self) -> VfsResult<Arc<T>> {
        self.ops
            .clone()
            .into_any()
            .downcast()
            .map_err(|_| VfsError::InvalidInput)
    }

    fn forget_removed_entry(entry: Option<DirEntry>) {
        if let Some(entry) = entry
            && let Ok(dir) = entry.as_dir()
        {
            dir.forget();
        }
    }

    fn lookup_locked(&self, name: &str, children: &mut DirChildren) -> VfsResult<DirEntry> {
        if !self.ops.is_cacheable() {
            return self.ops.lookup(name);
        }

        use hashbrown::hash_map::Entry;
        match children.entry(name.to_owned()) {
            Entry::Occupied(e) => Ok(e.get().clone()),
            Entry::Vacant(e) => {
                let node = self.ops.lookup(name)?;
                if self.ops.is_cacheable() {
                    e.insert(node.clone());
                }
                Ok(node)
            }
        }
    }

    /// Looks up a directory entry by name.
    pub fn lookup(&self, name: &str) -> VfsResult<DirEntry> {
        if name.len() > MAX_NAME_LEN {
            return Err(VfsError::NameTooLong);
        }
        // Fast path
        if self.ops.is_cacheable() {
            self.lookup_locked(name, &mut self.cache.lock())
        } else {
            self.ops.lookup(name)
        }
    }

    /// Looks up a directory entry by name in cache.
    pub fn lookup_cache(&self, name: &str) -> Option<DirEntry> {
        if self.ops.is_cacheable() {
            self.cache.lock().get(name).cloned()
        } else {
            None
        }
    }

    /// Inserts a directory entry into the cache.
    pub fn insert_cache(&self, name: String, entry: DirEntry) -> Option<DirEntry> {
        if self.ops.is_cacheable() {
            self.cache.lock().insert(name, entry)
        } else {
            None
        }
    }

    pub fn read_dir(&self, offset: u64, sink: &mut dyn DirEntrySink) -> VfsResult<usize> {
        self.ops.read_dir(offset, sink)
    }

    /// Creates a link to a node.
    pub fn link(&self, name: &str, node: &DirEntry) -> VfsResult<DirEntry> {
        verify_entry_name(name)?;

        self.ops.link(name, node).inspect(|entry| {
            // Hard links must share the same page cache (user_data) as the
            // source node.  Without this, in-memory filesystems like tmpfs
            // would create a new empty page cache for the link, losing the
            // file content.
            let user_data = node.user_data().clone();
            *entry.user_data() = user_data;
            if self.ops.is_cacheable() {
                self.cache.lock().insert(name.to_owned(), entry.clone());
            }
        })
    }

    /// Unlinks a directory entry by name.
    pub fn unlink(&self, name: &str, is_dir: bool) -> VfsResult<()> {
        verify_entry_name(name)?;

        let removed = {
            let mut children = self.cache.lock();
            let entry = self.lookup_locked(name, &mut children)?;
            match (entry.is_dir(), is_dir) {
                (true, false) => return Err(VfsError::IsADirectory),
                (false, true) => return Err(VfsError::NotADirectory),
                _ => {}
            }

            self.ops.unlink(name)?;
            children.remove(name)
        };
        Self::forget_removed_entry(removed);
        Ok(())
    }

    /// Returns whether the directory contains children.
    pub fn has_children(&self) -> VfsResult<bool> {
        self.ops.has_children()
    }

    fn create_locked(
        &self,
        name: &str,
        node_type: NodeType,
        permission: NodePermission,
        uid: u32,
        gid: u32,
        children: &mut DirChildren,
    ) -> VfsResult<DirEntry> {
        let entry = self.ops.create(name, node_type, permission, uid, gid)?;
        if self.ops.is_cacheable() {
            children.insert(name.to_owned(), entry.clone());
        }
        Ok(entry)
    }

    /// Creates a directory entry.
    pub fn create(
        &self,
        name: &str,
        node_type: NodeType,
        permission: NodePermission,
        uid: u32,
        gid: u32,
    ) -> VfsResult<DirEntry> {
        verify_entry_name(name)?;
        self.create_locked(
            name,
            node_type,
            permission,
            uid,
            gid,
            &mut self.cache.lock(),
        )
    }

    fn lock_both_cache<'a>(&'a self, other: &'a Self) -> LockedDirCaches<'a> {
        if core::ptr::eq(self, other) {
            return LockedDirCaches::Same {
                children: self.cache.lock(),
            };
        }

        let src_addr = &self.cache as *const _ as usize;
        let dst_addr = &other.cache as *const _ as usize;
        if src_addr < dst_addr {
            let src = lock_dir_cache(&self.cache, 0);
            let dst = lock_dir_cache(&other.cache, DIR_CACHE_NESTED_LOCK_SUBCLASS);
            LockedDirCaches::SrcThenDst { dst, src }
        } else {
            let dst = lock_dir_cache(&other.cache, 0);
            let src = lock_dir_cache(&self.cache, DIR_CACHE_NESTED_LOCK_SUBCLASS);
            LockedDirCaches::DstThenSrc { src, dst }
        }
    }

    /// Renames a directory entry.
    pub fn rename(&self, src_name: &str, dst_dir: &Self, dst_name: &str) -> VfsResult<()> {
        verify_entry_name(src_name)?;
        verify_entry_name(dst_name)?;

        let mut caches = self.lock_both_cache(dst_dir);

        let src = self.lookup_locked(src_name, caches.src_mut())?;
        if let Ok(dst) = dst_dir.lookup_locked(dst_name, caches.dst_mut()) {
            if src.node_type() == NodeType::Directory {
                if let Ok(dir) = dst.as_dir()
                    && dir.has_children()?
                {
                    return Err(VfsError::DirectoryNotEmpty);
                }
            } else if dst.node_type() == NodeType::Directory {
                return Err(VfsError::IsADirectory);
            }
        }
        drop(caches);

        self.ops.rename(src_name, dst_dir, dst_name).inspect(|_| {
            let (src_entry, prev_entry) = {
                let mut caches = self.lock_both_cache(dst_dir);
                let src_entry = caches.src_mut().remove(src_name);
                let prev_entry = caches.dst_mut().remove(dst_name);
                (src_entry, prev_entry)
            };

            Self::forget_removed_entry(prev_entry);

            if let Some(entry) = src_entry
                && dst_dir.ops.is_cacheable()
                && let Ok(fresh_entry) = dst_dir.ops.lookup(dst_name)
            {
                let user_data = {
                    let mut source = entry.user_data();
                    mem::take(source.deref_mut())
                };
                *fresh_entry.user_data().deref_mut() = user_data;
                dst_dir
                    .cache
                    .lock()
                    .insert(dst_name.to_owned(), fresh_entry);
            }
        })
    }

    /// Opens (or creates) a file in the directory.
    pub fn open_file(&self, name: &str, options: &OpenOptions) -> VfsResult<DirEntry> {
        verify_entry_name(name)?;

        let mut children = self.cache.lock();
        match self.lookup_locked(name, &mut children) {
            Ok(val) => {
                if options.create_new {
                    return Err(VfsError::AlreadyExists);
                }
                return Ok(val);
            }
            Err(err) if err.canonicalize() == VfsError::NotFound && options.create => {}
            Err(err) => return Err(err),
        }
        let (uid, gid) = options.user.unwrap_or((0, 0));
        let entry = self.create_locked(
            name,
            options.node_type,
            options.permission,
            uid,
            gid,
            &mut children,
        )?;
        Ok(entry)
    }

    pub fn mountpoint(&self) -> Option<Arc<Mountpoint>> {
        self.mountpoint.lock().clone()
    }

    pub fn is_mountpoint(&self) -> bool {
        self.mountpoint.lock().is_some()
    }

    /// Clears the cache of directory entries & user data, allowing them to be
    /// released.
    pub(crate) fn forget(&self) {
        let children = mem::take(self.cache.lock().deref_mut());
        for (_, child) in children {
            if let Ok(dir) = child.as_dir() {
                dir.forget();
            }
        }
    }
}
