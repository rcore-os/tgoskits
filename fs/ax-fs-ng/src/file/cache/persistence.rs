//! Ownership of backing/editor changes after their pages become clean.

#[cfg(feature = "ext4")]
use alloc::{sync::Arc, vec::Vec};

#[cfg(feature = "ext4")]
use axfs_ng_vfs::FilesystemOps;
use axfs_ng_vfs::{RawSpinLock, VfsError, VfsResult};

#[cfg(feature = "ext4")]
use super::{CachedFileShared, filesystem_key};

struct PersistenceState {
    version: u64,
    writers: usize,
    pending: bool,
}

pub(super) struct BackingPersistence {
    state: RawSpinLock<PersistenceState>,
}

pub(super) struct BackingChange<'a> {
    persistence: &'a BackingPersistence,
    version: u64,
    finished: bool,
}

impl BackingPersistence {
    pub(super) fn new() -> Self {
        Self {
            state: RawSpinLock::new(PersistenceState {
                version: 0,
                writers: 0,
                pending: false,
            }),
        }
    }

    pub(super) fn begin(&self) -> VfsResult<BackingChange<'_>> {
        let mut state = self.state.lock();
        let version = state
            .version
            .checked_add(1)
            .ok_or(VfsError::ValueOverflow)?;
        let writers = state
            .writers
            .checked_add(1)
            .ok_or(VfsError::ValueOverflow)?;
        state.version = version;
        state.writers = writers;
        state.pending = true;
        Ok(BackingChange {
            persistence: self,
            version,
            finished: false,
        })
    }

    pub(super) fn is_persisted(&self) -> bool {
        let state = self.state.lock();
        !state.pending && state.writers == 0
    }

    #[cfg(feature = "ext4")]
    pub(super) fn observe(&self) -> Option<u64> {
        let state = self.state.lock();
        (state.writers == 0).then_some(state.version)
    }

    #[cfg(feature = "ext4")]
    pub(super) fn acknowledge(&self, version: Option<u64>) {
        let mut state = self.state.lock();
        if state.writers == 0 && version == Some(state.version) {
            state.pending = false;
        }
    }
}

impl BackingChange<'_> {
    /// Called only after the backend/editor has explicitly synchronized.
    pub(super) fn complete_sync(mut self) {
        self.finish(true);
    }

    fn finish(&mut self, synchronized: bool) {
        if self.finished {
            return;
        }
        let mut state = self.persistence.state.lock();
        state.writers -= 1;
        if synchronized && state.writers == 0 && state.version == self.version {
            state.pending = false;
        }
        self.finished = true;
    }
}

impl Drop for BackingChange<'_> {
    fn drop(&mut self) {
        // Failure, partial I/O, unwinding, or a pages-only round leaves pending
        // ownership intact. This releases bookkeeping, never performs I/O.
        self.finish(false);
    }
}

/// A finite observation taken before the owning filesystem persists its state.
/// Dropping it after an error leaves every pending backing owner registered.
#[cfg(feature = "ext4")]
pub(crate) struct FilesystemPersistence {
    files: Vec<(Arc<CachedFileShared>, Option<u64>)>,
}

#[cfg(feature = "ext4")]
impl FilesystemPersistence {
    pub(super) fn observe(
        files: Vec<Arc<CachedFileShared>>,
        filesystem: &dyn FilesystemOps,
    ) -> Self {
        let key = filesystem_key(filesystem);
        let files = files
            .into_iter()
            .filter(|file| {
                file.backing
                    .as_ref()
                    .is_some_and(|backing| filesystem_key(backing.filesystem()) == key)
            })
            .map(|file| {
                let version = file.backing_persistence.observe();
                (file, version)
            })
            .collect();
        Self { files }
    }

    pub(crate) fn complete(self) {
        for (file, version) in self.files {
            file.backing_persistence.acknowledge(version);
        }
    }
}
