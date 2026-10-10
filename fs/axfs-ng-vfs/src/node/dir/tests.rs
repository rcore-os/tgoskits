//! Deterministic backend interleavings exercise the actual DirNode cache.

use alloc::{boxed::Box, string::String, sync::Arc};
use core::{
    any::Any,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

use hashbrown::HashMap;

use super::*;
use crate::{FilesystemOps, Metadata, MetadataUpdate, Reference, StatFs};

#[test]
fn non_not_found_errors_are_never_cached() {
    let (directory, backend) = fixture();
    for error in [
        VfsError::Io,
        VfsError::PermissionDenied,
        VfsError::InvalidData,
    ] {
        *backend.lookup_error.lock() = error;
        let before = backend.lookups.load(Ordering::Relaxed);
        for _ in 0..2 {
            assert!(matches!(directory.lookup("missing"), Err(actual) if actual == error));
        }
        assert_eq!(backend.lookups.load(Ordering::Relaxed), before + 2);
    }
}

#[test]
fn mixed_directory_revalidates_only_dynamic_children() {
    let backend = Arc::new(ScriptedDirectory {
        policy: CachePolicy::Mixed,
        ..Default::default()
    });
    let directory = DirNode::new(backend.clone());
    let stable = create_directory(&directory, "stable");
    create_directory(&directory, "volatile");

    assert!(directory.lookup("stable").unwrap().ptr_eq(&stable));
    assert!(directory.lookup("stable").unwrap().ptr_eq(&stable));
    assert_eq!(backend.lookups.load(Ordering::Relaxed), 0);

    assert!(directory.lookup_cache("volatile").is_none());
    directory.lookup("volatile").unwrap();
    directory.lookup("volatile").unwrap();
    assert_missing(&directory, "volatile-missing");
    assert_missing(&directory, "volatile-missing");
    assert_eq!(backend.lookups.load(Ordering::Relaxed), 4);
}

#[test]
fn cache_clear_rejects_an_in_flight_positive_lookup() {
    let (directory, backend) = fixture();
    let child = create_directory(&directory, "present");
    directory.clear_cached_entries();
    let changed = directory.clone();
    *backend.after_lookup.lock() = Some(Box::new(move || changed.clear_cached_entries()));
    assert!(directory.lookup("present").unwrap().ptr_eq(&child));
    assert!(directory.lookup_cache("present").is_none());
}

#[test]
fn failed_mutation_rejects_an_in_flight_positive_lookup() {
    let (directory, backend) = fixture();
    let preserved = create_directory(&directory, "preserved");
    let stale = create_directory(&directory, "stale");
    directory.remove_cache_after_mutation("stale");
    backend.fail_after_create.store(true, Ordering::Relaxed);
    let changed = directory.clone();
    *backend.after_lookup.lock() = Some(Box::new(move || {
        assert!(matches!(
            changed.create(
                "created",
                NodeType::Directory,
                NodePermission::default(),
                0,
                0
            ),
            Err(VfsError::Io)
        ));
    }));
    assert!(directory.lookup("stale").unwrap().ptr_eq(&stale));
    assert!(directory.lookup_cache("stale").is_none());
    assert!(directory.lookup("created").is_ok());
    assert!(directory.lookup("preserved").unwrap().ptr_eq(&preserved));
}

fn fixture() -> (Arc<DirNode>, Arc<ScriptedDirectory>) {
    let backend = Arc::new(ScriptedDirectory::default());
    (Arc::new(DirNode::new(backend.clone())), backend)
}

fn create_directory(directory: &DirNode, name: &str) -> DirEntry {
    directory
        .create(name, NodeType::Directory, NodePermission::default(), 0, 0)
        .unwrap()
}

fn assert_missing(directory: &DirNode, name: &str) {
    assert!(matches!(directory.lookup(name), Err(VfsError::NotFound)));
}

#[derive(Default)]
enum CachePolicy {
    #[default]
    Standard,
    Mixed,
}

struct ScriptedDirectory {
    entries: RawSpinLock<HashMap<String, DirEntry>>,
    lookups: AtomicUsize,
    lookup_error: RawSpinLock<VfsError>,
    after_lookup: RawSpinLock<Option<Box<dyn FnOnce() + Send>>>,
    fail_after_create: AtomicBool,
    policy: CachePolicy,
}

impl Default for ScriptedDirectory {
    fn default() -> Self {
        Self {
            entries: RawSpinLock::new(HashMap::new()),
            lookups: AtomicUsize::new(0),
            lookup_error: RawSpinLock::new(VfsError::NotFound),
            after_lookup: RawSpinLock::new(None),
            fail_after_create: AtomicBool::new(false),
            policy: CachePolicy::default(),
        }
    }
}

impl NodeOps for ScriptedDirectory {
    fn inode(&self) -> u64 {
        1
    }
    fn metadata(&self) -> VfsResult<Metadata> {
        Err(VfsError::Unsupported)
    }
    fn update_metadata(&self, _: MetadataUpdate) -> VfsResult<()> {
        Err(VfsError::Unsupported)
    }
    fn filesystem(&self) -> &dyn FilesystemOps {
        &ScriptedFilesystem
    }
    fn sync(&self, _: bool) -> VfsResult<()> {
        Ok(())
    }
    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }
}

impl DirNodeOps for ScriptedDirectory {
    fn is_cacheable_child(&self, name: &str) -> bool {
        !matches!(self.policy, CachePolicy::Mixed) || !name.starts_with("volatile")
    }
    fn lookup(&self, name: &str) -> VfsResult<DirEntry> {
        self.lookups.fetch_add(1, Ordering::Relaxed);
        let result = self
            .entries
            .lock()
            .get(name)
            .cloned()
            .ok_or(*self.lookup_error.lock());
        let callback = self.after_lookup.lock().take();
        if let Some(callback) = callback {
            callback();
        }
        result
    }

    fn create(
        &self,
        name: &str,
        _: NodeType,
        _: NodePermission,
        _: u32,
        _: u32,
    ) -> VfsResult<DirEntry> {
        let entry = DirEntry::new_dir(
            |_| DirNode::new(Arc::new(Self::default())),
            Reference::root(),
        );
        self.entries.lock().insert(name.into(), entry.clone());
        if self.fail_after_create.load(Ordering::Relaxed) {
            Err(VfsError::Io)
        } else {
            Ok(entry)
        }
    }

    fn read_dir(&self, _: DirectoryCursor, _: &mut dyn DirEntrySink) -> VfsResult<usize> {
        Err(VfsError::Unsupported)
    }
    fn create_symlink(
        &self,
        _: &str,
        _: &str,
        _: NodePermission,
        _: u32,
        _: u32,
    ) -> VfsResult<DirEntry> {
        Err(VfsError::Unsupported)
    }
    fn link(&self, _: &str, _: &DirEntry) -> VfsResult<DirEntry> {
        Err(VfsError::Unsupported)
    }
    fn unlink(&self, _: &str, _: bool) -> VfsResult<()> {
        Err(VfsError::Unsupported)
    }
    fn rename(&self, _: &str, _: &DirNode, _: &str, _: RenameOptions) -> VfsResult<()> {
        Err(VfsError::Unsupported)
    }
}

#[derive(Debug)]
struct ScriptedFilesystem;

impl FilesystemOps for ScriptedFilesystem {
    fn name(&self) -> &str {
        "scripted-directory"
    }
    fn root_dir(&self) -> DirEntry {
        panic!("not used by directory cache tests")
    }
    fn stat(&self) -> VfsResult<StatFs> {
        Err(VfsError::Unsupported)
    }
}
