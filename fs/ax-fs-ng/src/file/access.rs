use alloc::collections::BTreeMap;

use axfs_ng_vfs::{FilesystemOps, Location, VfsError, VfsResult};

use crate::os::sync::SleepMutex;

/// Prevents execution while a regular file is open for writing.
///
/// Share this lease through `Arc` when an open file description is retained by
/// a mapping. The last owner releases write access, including on failed opens.
pub struct WriteAccess {
    _lease: AccessLease,
}

impl WriteAccess {
    /// Acquires write access, failing with `TextFileBusy` if execution holds it.
    /// Acquisition and destruction may block on the access registry mutex.
    pub fn acquire(location: Location) -> VfsResult<Self> {
        Ok(Self {
            _lease: AccessLease::acquire(location, AccessKind::Write)?,
        })
    }
}

/// Keeps an executable inode unavailable to new writers until the last owner
/// drops its lease. Share through `Arc` when an address space is copied.
pub struct ExecutableFile {
    _lease: AccessLease,
}

impl ExecutableFile {
    /// Acquires execution access, failing with `TextFileBusy` if writers exist.
    /// Acquisition and destruction may block on the access registry mutex.
    pub fn acquire(location: Location) -> VfsResult<Self> {
        Ok(Self {
            _lease: AccessLease::acquire(location, AccessKind::Execute)?,
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AccessKind {
    Write,
    Execute,
}

struct AccessCount {
    kind: AccessKind,
    count: usize,
}

// The registry only stores counts. No filesystem callback or Location drop runs
// under its mutex. Each lease pins the filesystem and inode, so pointer/inode
// reuse cannot alias a live key; hard links and bind mounts share the same key.
static ACCESS: SleepMutex<BTreeMap<(usize, u64), AccessCount>> = SleepMutex::new(BTreeMap::new());

struct AccessLease {
    _location: Location,
    key: (usize, u64),
}

impl AccessLease {
    fn acquire(location: Location, kind: AccessKind) -> VfsResult<Self> {
        let filesystem = location.filesystem() as *const dyn FilesystemOps as *const () as usize;
        let key = (filesystem, location.inode());
        {
            let mut access = ACCESS.lock();
            let state = access.entry(key).or_insert(AccessCount { kind, count: 0 });
            if state.kind != kind {
                return Err(VfsError::TextFileBusy);
            }
            state.count = state.count.checked_add(1).ok_or(VfsError::ValueOverflow)?;
        }
        Ok(Self {
            _location: location,
            key,
        })
    }
}

impl Drop for AccessLease {
    fn drop(&mut self) {
        let mut access = ACCESS.lock();
        // Every constructed lease has exactly one registry reference.
        let state = access.get_mut(&self.key).expect("live file access lease");
        state.count -= 1;
        if state.count == 0 {
            access.remove(&self.key);
        }
        // `location` is dropped after this mutex guard, outside the registry.
    }
}
