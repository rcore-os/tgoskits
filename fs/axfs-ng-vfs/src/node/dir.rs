use alloc::{borrow::ToOwned, boxed::Box, string::String, sync::Arc};
use core::{
    any::Any,
    mem,
    ops::{Deref, DerefMut},
    sync::atomic::{AtomicU64, Ordering},
};

use hashbrown::HashMap;

use super::{DirEntry, FileExtentMap, FileExtentTarget};
use crate::{
    Mountpoint, Mutex, NodeOps, NodePermission, NodeType, VfsError, VfsResult,
    path::{DOT, DOTDOT, verify_entry_name},
};

/// A trait for a sink that can receive directory entries.
pub trait DirEntrySink {
    /// Accept a directory entry, returns `false` if the sink is full.
    ///
    /// `cursor` identifies the next entry to be read. Its continuation is
    /// backend-private and must be preserved by an open directory handle.
    ///
    /// It's not recommended to operate on the node inside the `accept`
    /// function, since some filesystem may impose a lock while iterating the
    /// directory, and operating on the node may cause deadlock.
    fn accept(
        &mut self,
        name: &[u8],
        ino: u64,
        node_type: NodeType,
        cursor: DirectoryCursor,
    ) -> bool;
}

impl<F: FnMut(&[u8], u64, NodeType, DirectoryCursor) -> bool> DirEntrySink for F {
    fn accept(
        &mut self,
        name: &[u8],
        ino: u64,
        node_type: NodeType,
        cursor: DirectoryCursor,
    ) -> bool {
        self(name, ino, node_type, cursor)
    }
}

/// Directory position shared between a filesystem and one open-directory
/// description.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DirectoryCursor {
    offset: u64,
    continuation: u64,
    observed_change_attribute: Option<u64>,
}

impl DirectoryCursor {
    pub const START: Self = Self::new(0);

    pub const fn new(offset: u64) -> Self {
        Self {
            offset,
            continuation: 0,
            observed_change_attribute: None,
        }
    }

    pub const fn with_continuation(offset: u64, continuation: u64) -> Self {
        Self {
            offset,
            continuation,
            observed_change_attribute: None,
        }
    }

    /// Associates filesystem-private continuation state with the directory
    /// change attribute against which it was produced.
    pub const fn with_observed_change_attribute(
        offset: u64,
        continuation: u64,
        change_attribute: u64,
    ) -> Self {
        Self {
            offset,
            continuation,
            observed_change_attribute: Some(change_attribute),
        }
    }

    pub const fn offset(self) -> u64 {
        self.offset
    }

    pub const fn continuation(self) -> u64 {
        self.continuation
    }

    pub const fn observed_change_attribute(self) -> Option<u64> {
        self.observed_change_attribute
    }
}

/// Opaque filesystem-private cache owned by one open directory description.
///
/// The explicit [`DirectoryCursor`] remains the authoritative position, so a
/// filesystem may populate or discard this state before an operation commits
/// its visible cursor.
pub trait DirectoryReadState: Send {
    fn as_any_mut(&mut self) -> &mut dyn Any;
}

impl<T: Any + Send> DirectoryReadState for T {
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

type DirChildren = HashMap<String, DirEntry>;

/// Typed filesystem rename behavior independent from Linux numeric flags.
///
/// Only valid `renameat2` combinations are constructible. `NOREPLACE` can be
/// combined with `WHITEOUT`, while `EXCHANGE` excludes both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenameOptions {
    no_replace: bool,
    exchange: bool,
    whiteout: bool,
}

impl RenameOptions {
    pub const REPLACE: Self = Self::new(false, false, false);
    pub const NO_REPLACE: Self = Self::new(true, false, false);
    pub const EXCHANGE: Self = Self::new(false, true, false);
    pub const WHITEOUT: Self = Self::new(false, false, true);
    pub const WHITEOUT_NO_REPLACE: Self = Self::new(true, false, true);

    const fn new(no_replace: bool, exchange: bool, whiteout: bool) -> Self {
        Self {
            no_replace,
            exchange,
            whiteout,
        }
    }

    pub const fn no_replace(self) -> bool {
        self.no_replace
    }

    pub const fn exchange(self) -> bool {
        self.exchange
    }

    pub const fn whiteout(self) -> bool {
        self.whiteout
    }
}

impl Default for RenameOptions {
    fn default() -> Self {
        Self::REPLACE
    }
}

pub trait DirNodeOps: NodeOps {
    /// Queries allocated mappings for a filesystem-backed directory inode.
    fn map_extents(
        &self,
        _offset: u64,
        _len: u64,
        _target: FileExtentTarget,
        _extent_limit: usize,
    ) -> VfsResult<FileExtentMap> {
        Err(VfsError::OperationNotSupported)
    }

    /// Reads directory entries.
    ///
    /// Returns the number of entries read.
    ///
    /// Implementations should ensure that `.` and `..` are present in the
    /// result.
    fn read_dir(&self, cursor: DirectoryCursor, sink: &mut dyn DirEntrySink) -> VfsResult<usize>;

    /// Creates private state for one open directory description.
    fn open_directory_read_state(&self) -> VfsResult<Box<dyn DirectoryReadState>> {
        Ok(Box::new(()))
    }

    /// Reads directory entries while retaining state owned by one open handle.
    fn read_dir_with_state(
        &self,
        _state: &mut dyn DirectoryReadState,
        cursor: DirectoryCursor,
        sink: &mut dyn DirEntrySink,
    ) -> VfsResult<usize> {
        self.read_dir(cursor, sink)
    }

    /// Returns the visible cursor used as the origin of `SEEK_END`.
    ///
    /// The default is the directory byte size. Filesystems whose directory
    /// positions are not byte offsets must expose their own terminal cookie.
    fn directory_end_cursor(&self) -> VfsResult<DirectoryCursor> {
        self.len().map(DirectoryCursor::new)
    }

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
        self.read_dir(DirectoryCursor::START, &mut |name: &[u8], _, _, _| {
            if name != DOT.as_bytes() && name != DOTDOT.as_bytes() {
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

    /// Atomically creates a symbolic link with its final target.
    ///
    /// Implementations must not publish an empty link before storing `target`.
    fn create_symlink(
        &self,
        name: &str,
        target: &str,
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
    fn unlink(&self, name: &str, is_dir: bool) -> VfsResult<()>;

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
    fn rename(
        &self,
        src_name: &str,
        dst_dir: &DirNode,
        dst_name: &str,
        options: RenameOptions,
    ) -> VfsResult<()>;
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
    cache_generation: AtomicU64,
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
            cache_generation: AtomicU64::new(0),
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

    fn lookup_and_cache(&self, name: &str) -> VfsResult<DirEntry> {
        if !self.ops.is_cacheable() {
            return self.ops.lookup(name);
        }

        let generation = self.cache_generation.load(Ordering::Acquire);
        if let Some(entry) = self.cache.lock().get(name).cloned() {
            return Ok(entry);
        }

        let node = self.ops.lookup(name)?;
        let mut cache = self.cache.lock();
        if self.cache_generation.load(Ordering::Acquire) != generation {
            return Ok(node);
        }

        use hashbrown::hash_map::Entry;
        Ok(match cache.entry(name.to_owned()) {
            Entry::Occupied(e) => e.get().clone(),
            Entry::Vacant(e) => e.insert(node).clone(),
        })
    }

    fn bump_cache_generation(&self) {
        self.cache_generation.fetch_add(1, Ordering::AcqRel);
    }

    fn remove_cache_after_mutation(&self, name: &str) -> Option<DirEntry> {
        if !self.ops.is_cacheable() {
            self.bump_cache_generation();
            return None;
        }

        {
            let mut cache = self.cache.lock();
            let removed = cache.remove(name);
            self.bump_cache_generation();
            removed
        }
    }

    /// Looks up a directory entry by name.
    pub fn lookup(&self, name: &str) -> VfsResult<DirEntry> {
        verify_entry_name(name)?;
        self.lookup_and_cache(name)
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
            let previous = self.cache.lock().insert(name, entry);
            self.bump_cache_generation();
            previous
        } else {
            None
        }
    }

    pub fn read_dir(
        &self,
        cursor: DirectoryCursor,
        sink: &mut dyn DirEntrySink,
    ) -> VfsResult<usize> {
        self.ops.read_dir(cursor, sink)
    }

    /// Creates filesystem-private state for one open directory description.
    pub fn open_directory_read_state(&self) -> VfsResult<Box<dyn DirectoryReadState>> {
        self.ops.open_directory_read_state()
    }

    /// Reads entries while retaining state owned by one open description.
    pub fn read_dir_with_state(
        &self,
        state: &mut dyn DirectoryReadState,
        cursor: DirectoryCursor,
        sink: &mut dyn DirEntrySink,
    ) -> VfsResult<usize> {
        self.ops.read_dir_with_state(state, cursor, sink)
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
                let previous = {
                    let mut cache = self.cache.lock();
                    cache.insert(name.to_owned(), entry.clone())
                };
                drop(previous);
                self.bump_cache_generation();
            }
        })
    }

    /// Unlinks a directory entry by name.
    pub fn unlink(&self, name: &str, is_dir: bool) -> VfsResult<()> {
        verify_entry_name(name)?;

        let entry = self.lookup(name)?;
        match (entry.is_dir(), is_dir) {
            (true, false) => return Err(VfsError::IsADirectory),
            (false, true) => return Err(VfsError::NotADirectory),
            _ => {}
        }

        self.ops.unlink(name, is_dir)?;
        let removed = self.remove_cache_after_mutation(name);
        Self::forget_removed_entry(removed);
        Ok(())
    }

    /// Returns whether the directory contains children.
    pub fn has_children(&self) -> VfsResult<bool> {
        self.ops.has_children()
    }

    fn create_entry(
        &self,
        name: &str,
        node_type: NodeType,
        permission: NodePermission,
        uid: u32,
        gid: u32,
    ) -> VfsResult<DirEntry> {
        if node_type == NodeType::Symlink {
            return Err(VfsError::InvalidInput);
        }
        let entry = self.ops.create(name, node_type, permission, uid, gid)?;
        if self.ops.is_cacheable() {
            let previous = {
                let mut cache = self.cache.lock();
                cache.insert(name.to_owned(), entry.clone())
            };
            drop(previous);
            self.bump_cache_generation();
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
        self.create_entry(name, node_type, permission, uid, gid)
    }

    /// Atomically creates a symbolic link with its final target.
    pub fn create_symlink(
        &self,
        name: &str,
        target: &str,
        permission: NodePermission,
        uid: u32,
        gid: u32,
    ) -> VfsResult<DirEntry> {
        verify_entry_name(name)?;
        let entry = self
            .ops
            .create_symlink(name, target, permission, uid, gid)?;
        if self.ops.is_cacheable() {
            let previous = {
                let mut cache = self.cache.lock();
                cache.insert(name.to_owned(), entry.clone())
            };
            drop(previous);
            self.bump_cache_generation();
        }
        Ok(entry)
    }

    fn transfer_cached_state(source: DirEntry, destination: &DirEntry) {
        let user_data = {
            let mut source_data = source.user_data();
            mem::take(source_data.deref_mut())
        };
        *destination.user_data().deref_mut() = user_data;
        if let (Ok(source_dir), Ok(destination_dir)) = (source.as_dir(), destination.as_dir()) {
            // Child entries retain parent references and must be looked up
            // again, but the mountpoint belongs to the moved directory.
            let mountpoint = mem::take(source_dir.mountpoint.lock().deref_mut());
            *destination_dir.mountpoint.lock().deref_mut() = mountpoint;
        }
    }

    fn update_cache_after_rename(
        &self,
        src_name: &str,
        dst_dir: &Self,
        dst_name: &str,
        options: RenameOptions,
    ) {
        let (source_entry, target_entry) =
            if core::ptr::eq(self, dst_dir) && self.ops.is_cacheable() {
                let mut children = self.cache.lock();
                let source = children.remove(src_name);
                let target = if src_name == dst_name {
                    None
                } else {
                    children.remove(dst_name)
                };
                self.bump_cache_generation();
                (source, target)
            } else {
                (
                    self.remove_cache_after_mutation(src_name),
                    dst_dir.remove_cache_after_mutation(dst_name),
                )
            };

        if options.exchange() {
            if let Some(source) = source_entry
                && dst_dir.ops.is_cacheable()
                && let Ok(fresh_target) = dst_dir.ops.lookup(dst_name)
            {
                Self::transfer_cached_state(source, &fresh_target);
                dst_dir.insert_cache(dst_name.to_owned(), fresh_target);
            }
            if let Some(target) = target_entry
                && self.ops.is_cacheable()
                && let Ok(fresh_source) = self.ops.lookup(src_name)
            {
                Self::transfer_cached_state(target, &fresh_source);
                self.insert_cache(src_name.to_owned(), fresh_source);
            }
            return;
        }

        Self::forget_removed_entry(target_entry);
        if let Some(source) = source_entry
            && dst_dir.ops.is_cacheable()
            && let Ok(fresh_destination) = dst_dir.ops.lookup(dst_name)
        {
            Self::transfer_cached_state(source, &fresh_destination);
            dst_dir.insert_cache(dst_name.to_owned(), fresh_destination);
        }
    }

    /// Renames a directory entry with ordinary replacement semantics.
    pub fn rename(&self, src_name: &str, dst_dir: &Self, dst_name: &str) -> VfsResult<()> {
        self.rename_with_options(src_name, dst_dir, dst_name, RenameOptions::REPLACE)
    }

    /// Renames a directory entry with typed `renameat2` behavior.
    pub fn rename_with_options(
        &self,
        src_name: &str,
        dst_dir: &Self,
        dst_name: &str,
        options: RenameOptions,
    ) -> VfsResult<()> {
        verify_entry_name(src_name)?;
        verify_entry_name(dst_name)?;

        let src = self.lookup(src_name)?;
        let destination = match dst_dir.lookup(dst_name) {
            Ok(destination) => Some(destination),
            Err(VfsError::NotFound) => None,
            Err(error) => return Err(error),
        };
        if options.no_replace() && destination.is_some() {
            return Err(VfsError::AlreadyExists);
        }
        if options.exchange() && destination.is_none() {
            return Err(VfsError::NotFound);
        }
        if !options.exchange()
            && let Some(destination) = &destination
        {
            match (
                src.node_type() == NodeType::Directory,
                destination.node_type() == NodeType::Directory,
            ) {
                (true, false) => return Err(VfsError::NotADirectory),
                (false, true) => return Err(VfsError::IsADirectory),
                (true, true) if destination.as_dir()?.has_children()? => {
                    return Err(VfsError::DirectoryNotEmpty);
                }
                _ => {}
            }
        }
        if !options.no_replace()
            && !options.whiteout()
            && destination
                .as_ref()
                .is_some_and(|target| target.inode() == src.inode())
        {
            return Ok(());
        }

        self.ops
            .rename(src_name, dst_dir, dst_name, options)
            .inspect(|_| self.update_cache_after_rename(src_name, dst_dir, dst_name, options))
    }

    /// Opens (or creates) a file in the directory.
    pub fn open_file(&self, name: &str, options: &OpenOptions) -> VfsResult<DirEntry> {
        verify_entry_name(name)?;

        match self.lookup(name) {
            Ok(val) => {
                if options.create_new {
                    return Err(VfsError::AlreadyExists);
                }
                return Ok(val);
            }
            Err(VfsError::NotFound) if options.create => {}
            Err(err) => return Err(err),
        }
        let (uid, gid) = options.user.unwrap_or((0, 0));
        let entry = match self.create_entry(name, options.node_type, options.permission, uid, gid) {
            Ok(entry) => entry,
            Err(VfsError::AlreadyExists) if !options.create_new => self.lookup(name)?,
            Err(err) => return Err(err),
        };
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
