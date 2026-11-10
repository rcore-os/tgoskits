//! Directory operations, namespace mutations, and cached name resolution.

use alloc::{borrow::ToOwned, boxed::Box, string::String, sync::Arc};
use core::{any::Any, ops::Deref};

use super::{DirEntry, FileExtentMap, FileExtentTarget};
use crate::{
    Mountpoint, NodeOps, NodePermission, NodeType, RawSpinLock, VfsError, VfsResult,
    path::{DOT, DOTDOT, verify_entry_name},
};

mod cache;
mod command;
mod mutation;
mod outcome;
use cache::DirectoryCache;
pub(crate) use command::MutationTarget;
pub use command::*;
pub use mutation::*;
pub use outcome::*;

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
    /// Identity of the owner that serializes namespace changes. Native shared
    /// backends override this with their filesystem owner. This key observes
    /// publication ordering; it does not grant exclusion or authorization.
    fn namespace_domain(&self) -> usize {
        core::ptr::from_ref(self) as *const () as usize
    }

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

    /// Canonical key of an authoritative lookup. Exact-name backends use the
    /// request itself; backends with aliases override this without changing VFS
    /// matching rules for unrelated filesystems.
    fn canonical_name(&self, name: &str, _entry: &DirEntry) -> String {
        name.to_owned()
    }

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

    /// Returns whether a child may be cached. Mixed directories can keep
    /// stable mount points cached while resolving changing entries on demand.
    fn is_cacheable_child(&self, _name: &str) -> bool {
        self.is_cacheable()
    }

    /// Returns a cache-only version for confirmed missing names, if supported.
    ///
    /// Opt in only when names use exact matching, lookup errors are independent
    /// of caller credentials, and every namespace change updates this version
    /// across all aliases of the directory inode. It must not perform I/O or
    /// wait for filesystem exclusion. Unavailable or transaction-private state
    /// returns `None`, forcing authoritative lookup. Errors must be propagated.
    /// This has no effect when [`Self::is_cacheable`] returns `false`.
    fn negative_cache_generation(&self) -> VfsResult<Option<u64>> {
        Ok(None)
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

    /// Runs a controlled command under the backend's real exclusion. Writable
    /// backends implement this by lending each locked attempt to the request's
    /// fixed executor. Read-only/synthetic directories gain no write capability.
    fn mutate_namespace(
        &self,
        _command: &NamespaceMutation<'_>,
        _retired: &MutationRetire,
    ) -> NamespaceOutcome<NamespaceEffect> {
        NamespaceOutcome::unchanged(Err(VfsError::OperationNotSupported))
    }
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
    /// Final-object DAC for an existing entry, empty for a path-only handle.
    pub access: NodePermission,
}

impl Default for OpenOptions {
    fn default() -> Self {
        Self {
            create: false,
            create_new: false,
            node_type: NodeType::RegularFile,
            permission: NodePermission::default(),
            user: None,
            access: NodePermission::empty(),
        }
    }
}

pub struct DirNode {
    ops: Arc<dyn DirNodeOps>,
    cache: RawSpinLock<Box<DirectoryCache>>,
    pub(crate) mountpoint: RawSpinLock<Option<Arc<Mountpoint>>>,
    owner: Option<crate::WeakDirEntry>,
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
            cache: RawSpinLock::new(Box::new(DirectoryCache::default())),
            mountpoint: RawSpinLock::new(None),
            owner: None,
        }
    }

    pub fn inner(&self) -> &Arc<dyn DirNodeOps> {
        &self.ops
    }

    pub(super) fn set_owner(&mut self, owner: crate::WeakDirEntry) {
        self.owner = Some(owner);
    }

    fn owner(&self) -> VfsResult<DirEntry> {
        self.owner
            .as_ref()
            .and_then(crate::WeakDirEntry::upgrade)
            .ok_or(VfsError::BadState)
    }

    fn ensure_linked(&self) -> VfsResult<()> {
        if let Some(owner) = self.owner.as_ref().and_then(crate::WeakDirEntry::upgrade) {
            match owner.name_state() {
                crate::NameState::Unlinked => return Err(VfsError::NotFound),
                crate::NameState::Untrusted => return Err(VfsError::WouldBlock),
                _ => {}
            }
        }
        Ok(())
    }

    pub fn downcast<T: DirNodeOps>(&self) -> VfsResult<Arc<T>> {
        self.ops
            .clone()
            .into_any()
            .downcast()
            .map_err(|_| VfsError::InvalidInput)
    }

    /// Looks up a directory entry by name.
    pub fn lookup(&self, name: &str) -> VfsResult<DirEntry> {
        verify_entry_name(name)?;
        self.lookup_and_cache(name)
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

    /// Returns whether the directory contains children.
    pub fn has_children(&self) -> VfsResult<bool> {
        self.ops.has_children()
    }

    /// Runs only requests privately constructed by a credentialed Location.
    pub fn mutate_namespace(
        &self,
        command: &NamespaceMutation<'_>,
        retired: &MutationRetire,
    ) -> NamespaceOutcome<NamespaceEffect> {
        if let Err(error) = self.ensure_linked() {
            return NamespaceOutcome::unchanged(Err(error));
        }
        self.ops.mutate_namespace(command, retired)
    }

    pub(super) fn publish_locked_command(
        &self,
        effect: &NamespaceEffect,
        retired: &MutationRetire,
    ) -> VfsResult<()> {
        match effect {
            NamespaceEffect::Create(effect) => {
                self.insert_locked_cache(
                    effect.selected.canonical_name.clone(),
                    &effect.selected.entry,
                    effect.selected.cacheable,
                    retired,
                );
            }
            NamespaceEffect::Link(alias) => {
                self.insert_locked_cache(
                    alias.canonical_name.clone(),
                    &alias.entry,
                    alias.cacheable,
                    retired,
                );
            }
            NamespaceEffect::Unlink(removed) => {
                let cached = self.remove_cache_after_mutation(&removed.canonical_name);
                if let Some(cached) = &cached {
                    cached.set_name_state(if cached.same_backend_object(&removed.entry) {
                        crate::NameState::Unlinked
                    } else {
                        crate::NameState::Untrusted
                    });
                }
                removed.entry.set_name_state(crate::NameState::Unlinked);
                retired.retire(cached);
            }
            NamespaceEffect::Rename(_) => return Err(VfsError::BadState),
        }
        Ok(())
    }

    pub(crate) fn publish_locked_rename(
        &self,
        destination: &Self,
        effect: &RenameEffect,
        retired: &MutationRetire,
    ) -> VfsResult<()> {
        if effect.action == RenameAction::Noop {
            return Ok(());
        }
        let source = self
            .locked_entry(&effect.source_name)
            .ok_or(VfsError::BadState)?;
        retired.retire(source.clone());
        if !effect.source.matches(&source) {
            return Err(VfsError::BadState);
        }
        let target = destination.locked_entry(&effect.destination_name);
        retired.retire(target.clone());
        if !match (&effect.target, &target) {
            (Some(actual), Some(entry)) => actual.matches(entry),
            (None, None) => true,
            _ => false,
        } {
            return Err(VfsError::BadState);
        }
        let old_parent = source.name_snapshot();
        let source_parent = old_parent.parent().cloned().ok_or(VfsError::BadState)?;
        retired.retire(old_parent);
        let destination_parent = destination.owner()?;
        retired.retire(self.remove_cache_after_mutation(&effect.source_name));
        retired.retire(destination.remove_cache_after_mutation(&effect.destination_name));
        if effect.action == RenameAction::Exchange {
            let target = target.as_ref().ok_or(VfsError::BadState)?;
            retired.retire(DirEntry::exchange_names(
                &source,
                destination_parent,
                effect.destination_name.clone(),
                target,
                source_parent,
                effect.source_name.clone(),
            ));
            self.insert_locked_cache(
                effect.source_name.clone(),
                target,
                effect.source_cacheable,
                retired,
            );
        } else {
            retired.retire(source.move_to(destination_parent, effect.destination_name.clone()));
            if let Some(target) = &target {
                target.set_name_state(crate::NameState::Unlinked);
            }
            retired.retire(source_parent);
        }
        destination.insert_locked_cache(
            effect.destination_name.clone(),
            &source,
            effect.destination_cacheable,
            retired,
        );
        Ok(())
    }

    pub fn mountpoint(&self) -> Option<Arc<Mountpoint>> {
        self.mountpoint.lock().clone()
    }

    pub fn is_mountpoint(&self) -> bool {
        self.mountpoint.lock().is_some()
    }
}

#[cfg(all(test, feature = "host-test"))]
mod tests;
