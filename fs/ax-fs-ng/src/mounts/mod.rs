//! Retain mounted filesystems until their shutdown actually succeeds.

use alloc::vec::Vec;

use axfs_ng_vfs::{Filesystem, VfsResult};

use crate::os::sync::Mutex;

// Registration and shutdown are task-context operations. The list lock is
// never held across filesystem callbacks; the separate owner serializes
// shutdown snapshots until failed owners have been restored to the list.
struct MountRegistry {
    filesystems: Mutex<Vec<Filesystem>>,
    shutdown_owner: Mutex<()>,
}

impl MountRegistry {
    const fn new() -> Self {
        Self {
            filesystems: Mutex::new(Vec::new()),
            shutdown_owner: Mutex::new(()),
        }
    }
}

static MOUNTED_FILESYSTEMS: MountRegistry = MountRegistry::new();

pub(crate) fn register_mounted_filesystem(filesystem: Filesystem) {
    MOUNTED_FILESYSTEMS.filesystems.lock().push(filesystem);
}

pub(crate) fn shutdown_registered_filesystems() -> VfsResult<()> {
    shutdown_registry(&MOUNTED_FILESYSTEMS)
}

fn shutdown_registry(registry: &MountRegistry) -> VfsResult<()> {
    let _owner = registry.shutdown_owner.lock();
    let filesystems = core::mem::take(&mut *registry.filesystems.lock());
    let mut first_error = None;
    let mut failed = Vec::with_capacity(filesystems.len());
    for fs in filesystems.into_iter().rev() {
        if let Err(error) = fs.shutdown() {
            first_error.get_or_insert(error);
            failed.push(fs);
        }
    }
    // Preserve mount order so a later shutdown still visits children first.
    // Callbacks and successful owner destruction have already completed
    // outside the registry lock. Concurrent registrations must not be lost.
    failed.reverse();
    let mut registered = registry.filesystems.lock();
    failed.append(&mut registered);
    core::mem::swap(&mut *registered, &mut failed);
    drop(registered);
    first_error.map_or(Ok(()), Err)
}

#[cfg(test)]
mod tests;

#[cfg(test)]
pub(crate) fn clear_registered_filesystems_for_test() {
    let detached = core::mem::take(&mut *MOUNTED_FILESYSTEMS.filesystems.lock());
    drop(detached);
}
