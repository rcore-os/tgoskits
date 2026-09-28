//! Canonical page-cache publication across ext4 hardlink dentries.

use alloc::{
    collections::BTreeMap,
    sync::{Arc, Weak},
};

use axfs_ng_vfs::{FilesystemOps, Location};

use super::{CachedFileShared, Mutex};

type CachedFileKey = (usize, u64);
type InodeCacheIndex = BTreeMap<CachedFileKey, Weak<CachedFileShared>>;

static CACHED_FILE_BY_INODE: Mutex<InodeCacheIndex> = Mutex::new(BTreeMap::new());

pub(super) fn key_for(location: &Location) -> Option<CachedFileKey> {
    (location.filesystem().name() == "ext4")
        .then(|| (filesystem_key(location.filesystem()), location.inode()))
}

pub(super) fn lookup(key: CachedFileKey) -> Option<Arc<CachedFileShared>> {
    CACHED_FILE_BY_INODE
        .lock()
        .get(&key)
        .and_then(Weak::upgrade)
}

pub(super) fn publish(
    key: CachedFileKey,
    candidate: Arc<CachedFileShared>,
) -> Arc<CachedFileShared> {
    let shared = publish_in(&mut CACHED_FILE_BY_INODE.lock(), key, &candidate);
    // The losing candidate owns a FileNode. Its destructor may acquire ext4
    // state, so it must run after releasing the index lock. Reap takes ext4
    // state before removing an index key.
    drop(candidate);
    shared
}

fn publish_in(
    index: &mut InodeCacheIndex,
    key: CachedFileKey,
    candidate: &Arc<CachedFileShared>,
) -> Arc<CachedFileShared> {
    if let Some(shared) = index.get(&key).and_then(Weak::upgrade) {
        shared
    } else {
        index.insert(key, Arc::downgrade(candidate));
        candidate.clone()
    }
}

/// The ext4 lifetime owner calls this with no live inode references, before
/// freeing the inode number. Unlink alone cannot retire an open file's cache.
pub(crate) fn forget_cached_file_key(filesystem: &dyn FilesystemOps, inode: u64) {
    CACHED_FILE_BY_INODE
        .lock()
        .remove(&(filesystem_key(filesystem), inode));
}

fn filesystem_key(filesystem: &dyn FilesystemOps) -> usize {
    filesystem as *const dyn FilesystemOps as *const () as usize
}

#[cfg(test)]
mod tests;
