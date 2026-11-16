use alloc::{collections::BTreeMap, vec::Vec};

use axfs_ng_vfs::{FilesystemOps, Location, PathHandle, PreparedWrite, VfsError, VfsResult};

use crate::os::sync::Mutex;

/// Prevents execution and read-only remount while a regular file is writable.
///
/// Share this lease through `Arc` when an open file description is retained by
/// a mapping. The last owner releases write access, including on failed opens.
pub struct WriteAccess {
    backing: Mutex<Option<BackingWriteAccess>>,
    _lease: AccessLease,
    // Release inode access before the complete graph's write authority; keep
    // the selected path use through retirement of every preparation owner.
    _write: PreparedWrite,
    _path: PathHandle,
}

struct BackingWriteAccess {
    _leases: Vec<AccessLease>,
    _paths: Vec<PathHandle>,
}

impl WriteAccess {
    pub(crate) fn bind_backing(&self, target: &axfs_ng_vfs::FileIoTarget) -> VfsResult<()> {
        let paths = &target.paths()[1..];
        let mut retained = Vec::new();
        let mut leases = Vec::new();
        retained
            .try_reserve_exact(paths.len())
            .map_err(|_| VfsError::NoMemory)?;
        leases
            .try_reserve_exact(paths.len())
            .map_err(|_| VfsError::NoMemory)?;
        for path in paths {
            self._write
                .write_for(path.location())?
                .check_location(path.location())?;
            retained.push(path.clone());
            leases.push(AccessLease::acquire(path, AccessKind::Write)?);
        }
        let backing = BackingWriteAccess {
            _leases: leases,
            _paths: retained,
        };
        let mut selected = self.backing.lock();
        if selected.is_some() {
            drop(selected);
            return Err(VfsError::BadState);
        }
        *selected = Some(backing);
        Ok(())
    }

    pub(crate) fn prepared_write(&self) -> &PreparedWrite {
        &self._write
    }

    pub(crate) fn prepare_open(
        &self,
        access: axfs_ng_vfs::NodePermission,
        credentials: &axfs_ng_vfs::MutationCredentials<'_>,
    ) -> VfsResult<()> {
        self._path
            .location()
            .prepare_write_open(&self._write, access, credentials)
    }

    /// Acquires the complete backing graph's rights and inode write access.
    /// Fails with `ReadOnlyFilesystem` for local/shared protection or
    /// `TextFileBusy` if execution holds the inode. Acquire before backend gates.
    /// Acquisition may sleep behind reconfiguration; destruction releases the
    /// inode registry before releasing any mount writer counts. Read-only
    /// dependencies retain their read role and require no write authority.
    pub fn acquire(location: Location) -> VfsResult<Self> {
        let path = PathHandle::acquire(location)?;
        let write = PreparedWrite::acquire(path.location())?;
        Ok(Self {
            backing: Mutex::new(None),
            _lease: AccessLease::acquire(&path, AccessKind::Write)?,
            _write: write,
            _path: path,
        })
    }
}

/// Keeps an executable inode unavailable to new writers until the last owner
/// drops its lease. Share through `Arc` when an address space is copied.
pub struct ExecutableFile {
    _lease: AccessLease,
    path: PathHandle,
}

impl ExecutableFile {
    /// Path pinned by this execution lease, shared by forked address spaces.
    pub fn location(&self) -> &Location {
        self.path.location()
    }

    /// Acquires execution access, failing with `TextFileBusy` if writers exist.
    /// Acquisition and destruction may block on the access registry mutex.
    pub fn acquire(location: Location) -> VfsResult<Self> {
        let path = PathHandle::acquire(location)?;
        Ok(Self {
            _lease: AccessLease::acquire(&path, AccessKind::Execute)?,
            path,
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
// under its mutex. Each owner retains its path after the lease, so pointer/inode
// reuse cannot alias a live key; hard links and bind mounts share the same key.
static ACCESS: Mutex<BTreeMap<(usize, u64), AccessCount>> = Mutex::new(BTreeMap::new());

struct AccessLease {
    key: (usize, u64),
}

impl AccessLease {
    fn acquire(path: &PathHandle, kind: AccessKind) -> VfsResult<Self> {
        let location = path.location();
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
        Ok(Self { key })
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
        // The owning WriteAccess/ExecutableFile retires its path after this
        // lease, outside the registry and after any backing write authority.
    }
}
