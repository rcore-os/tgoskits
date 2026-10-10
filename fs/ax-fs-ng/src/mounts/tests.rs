use alloc::{sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use axfs_ng_vfs::{DirEntry, FilesystemOps, StatFs, VfsError};

use super::*;

#[test]
fn shutdown_retains_failures_in_mount_order_and_retries_only_those() {
    let registry = Arc::new(MountRegistry::new());
    let calls = Arc::new(Mutex::new(Vec::new()));
    let first = test_filesystem("first", &registry, &calls);
    let middle = test_filesystem("middle", &registry, &calls);
    let last = test_filesystem("last", &registry, &calls);
    first.fail.store(true, Ordering::Release);
    last.fail.store(true, Ordering::Release);
    registry.filesystems.lock().extend([
        Filesystem::new(first.clone()),
        Filesystem::new(middle.clone()),
        Filesystem::new(last.clone()),
    ]);
    let middle_lifetime = Arc::downgrade(&middle);
    let first_lifetime = Arc::downgrade(&first);
    let last_lifetime = Arc::downgrade(&last);
    drop((first, middle, last));

    assert_eq!(shutdown_registry(&registry), Err(VfsError::Io));
    assert_eq!(*calls.lock().unwrap(), ["last", "middle", "first"]);
    let retained: Vec<_> = registry
        .filesystems
        .lock()
        .iter()
        .map(|fs| fs.name().to_owned())
        .collect();
    assert_eq!(retained, ["first", "last"]);
    assert!(middle_lifetime.upgrade().is_none());
    first_lifetime
        .upgrade()
        .unwrap()
        .fail
        .store(false, Ordering::Release);
    last_lifetime
        .upgrade()
        .unwrap()
        .fail
        .store(false, Ordering::Release);

    shutdown_registry(&registry).unwrap();
    assert!(registry.filesystems.lock().is_empty());
    assert!(registry.shutdown_owner.try_lock().is_some());
    assert_eq!(
        *calls.lock().unwrap(),
        ["last", "middle", "first", "last", "first"]
    );
    assert!(first_lifetime.upgrade().is_none());
    assert!(last_lifetime.upgrade().is_none());
}

#[test]
fn shutdown_preserves_mounts_registered_by_an_unlocked_callback() {
    let registry = Arc::new(MountRegistry::new());
    let calls = Arc::new(Mutex::new(Vec::new()));
    let original = test_filesystem("original", &registry, &calls);
    let added = test_filesystem("added", &registry, &calls);
    original.fail.store(true, Ordering::Release);
    *original.register_on_shutdown.lock().unwrap() = Some(Filesystem::new(added));
    registry.filesystems.lock().push(Filesystem::new(original));

    assert_eq!(shutdown_registry(&registry), Err(VfsError::Io));
    let retained: Vec<_> = registry
        .filesystems
        .lock()
        .iter()
        .map(|fs| fs.name().to_owned())
        .collect();
    assert_eq!(retained, ["original", "added"]);
    assert_eq!(*calls.lock().unwrap(), ["original"]);
    // Break fixture ownership cycles without touching the production registry.
    assert!(registry.shutdown_owner.try_lock().is_some());
    let filesystems = core::mem::take(&mut *registry.filesystems.lock());
    drop(filesystems);
}

struct ShutdownFilesystem {
    name: &'static str,
    fail: AtomicBool,
    registry: Arc<MountRegistry>,
    calls: Arc<Mutex<Vec<&'static str>>>,
    register_on_shutdown: Mutex<Option<Filesystem>>,
}

fn test_filesystem(
    name: &'static str,
    registry: &Arc<MountRegistry>,
    calls: &Arc<Mutex<Vec<&'static str>>>,
) -> Arc<ShutdownFilesystem> {
    Arc::new(ShutdownFilesystem {
        name,
        fail: AtomicBool::new(false),
        registry: registry.clone(),
        calls: calls.clone(),
        register_on_shutdown: Mutex::new(None),
    })
}

impl FilesystemOps for ShutdownFilesystem {
    fn name(&self) -> &str {
        self.name
    }

    fn root_dir(&self) -> DirEntry {
        panic!("shutdown must not create a new root inode")
    }

    fn stat(&self) -> VfsResult<StatFs> {
        Err(VfsError::Unsupported)
    }

    fn shutdown(&self) -> VfsResult<()> {
        assert!(
            self.registry.filesystems.try_lock().is_some(),
            "shutdown holds the registration lock"
        );
        assert!(
            self.registry.shutdown_owner.try_lock().is_none(),
            "a second shutdown can mistake the in-flight snapshot for an empty registry"
        );
        self.calls.lock().unwrap().push(self.name);
        if let Some(filesystem) = self.register_on_shutdown.lock().unwrap().take() {
            self.registry.filesystems.lock().push(filesystem);
        }
        if self.fail.load(Ordering::Acquire) {
            Err(VfsError::Io)
        } else {
            Ok(())
        }
    }
}
