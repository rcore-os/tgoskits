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
fn missing_names_are_cached_without_evicting_positive_owners() {
    let (directory, backend) = fixture();
    let child = create_directory(&directory, "present");
    child.user_data().insert(42_u32);
    assert_missing(&directory, "missing");
    let before = backend.lookups.load(Ordering::Relaxed);
    assert_missing(&directory, "missing");
    assert_eq!(backend.lookups.load(Ordering::Relaxed), before);

    for index in 0..1024 {
        assert_missing(&directory, &alloc::format!("missing-{index}"));
    }
    let before = backend.lookups.load(Ordering::Relaxed);
    assert_missing(&directory, "missing");
    assert_eq!(backend.lookups.load(Ordering::Relaxed), before + 1);
    let cached = directory.lookup("present").unwrap();
    assert!(cached.ptr_eq(&child));
    assert_eq!(*cached.user_data().get::<u32>().unwrap(), 42);
}

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
fn opted_out_and_uncacheable_directories_revalidate_missing_names() {
    for policy in [CachePolicy::PositiveOnly, CachePolicy::Disabled] {
        let backend = Arc::new(ScriptedDirectory {
            policy,
            ..Default::default()
        });
        let directory = DirNode::new(backend.clone());
        assert_missing(&directory, "missing");
        assert_missing(&directory, "missing");
        assert_eq!(backend.lookups.load(Ordering::Relaxed), 2);
    }
}

#[test]
fn stale_missing_lookup_cannot_hide_a_completed_create() {
    let (directory, backend) = fixture();
    let changed = directory.clone();
    *backend.after_lookup.lock() = Some(Box::new(move || {
        create_directory(&changed, "created");
    }));
    assert_missing(&directory, "created");
    assert!(directory.lookup("created").is_ok());
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
fn cache_clear_rejects_an_in_flight_missing_lookup() {
    let (directory, backend) = fixture();
    let changed = directory.clone();
    *backend.after_lookup.lock() = Some(Box::new(move || changed.clear_cached_entries()));
    assert_missing(&directory, "missing");
    assert_missing(&directory, "missing");
    assert_eq!(backend.lookups.load(Ordering::Relaxed), 2);
}

#[test]
fn failed_mutation_invalidates_missing_results_without_losing_positive_owners() {
    let (directory, backend) = fixture();
    let preserved = create_directory(&directory, "preserved");
    assert_missing(&directory, "created");
    backend.fail_after_create.store(true, Ordering::Relaxed);
    assert!(matches!(
        directory.create(
            "created",
            NodeType::Directory,
            NodePermission::default(),
            0,
            0
        ),
        Err(VfsError::Io)
    ));
    assert!(directory.lookup("created").is_ok());
    assert!(directory.lookup("preserved").unwrap().ptr_eq(&preserved));
}

#[test]
fn successful_create_replaces_a_cached_missing_name() {
    let (directory, _) = fixture();
    assert_missing(&directory, "created");
    let child = create_directory(&directory, "created");
    assert!(directory.lookup("created").unwrap().ptr_eq(&child));
}

#[test]
fn a_second_directory_alias_invalidates_cached_missing_names() {
    let (directory, backend) = fixture();
    let alias = DirNode::new(backend);
    assert_missing(&directory, "created");
    let child = create_directory(&alias, "created");
    assert!(directory.lookup("created").unwrap().ptr_eq(&child));
}

#[test]
fn unavailable_generation_forces_authoritative_lookup() {
    let (directory, backend) = fixture();
    assert_missing(&directory, "missing");
    *backend.generation.lock() = Ok(None);
    let before = backend.lookups.load(Ordering::Relaxed);
    assert_missing(&directory, "missing");
    assert_missing(&directory, "missing");
    assert_eq!(backend.lookups.load(Ordering::Relaxed), before + 2);
    *backend.generation.lock() = Err(VfsError::Io);
    assert!(matches!(directory.lookup("missing"), Err(VfsError::Io)));
}

#[test]
fn an_alias_mutation_during_lookup_prevents_negative_publication() {
    let (directory, backend) = fixture();
    let alias = DirNode::new(backend.clone());
    *backend.after_lookup.lock() = Some(Box::new(move || {
        create_directory(&alias, "created");
    }));
    assert_missing(&directory, "created");
    assert!(directory.lookup("created").is_ok());
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
    PositiveAndNegative,
    PositiveOnly,
    Disabled,
}

struct ScriptedDirectory {
    entries: Mutex<HashMap<String, DirEntry>>,
    lookups: AtomicUsize,
    lookup_error: Mutex<VfsError>,
    after_lookup: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    fail_after_create: AtomicBool,
    policy: CachePolicy,
    generation: Mutex<VfsResult<Option<u64>>>,
}

impl Default for ScriptedDirectory {
    fn default() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            lookups: AtomicUsize::new(0),
            lookup_error: Mutex::new(VfsError::NotFound),
            after_lookup: Mutex::new(None),
            fail_after_create: AtomicBool::new(false),
            policy: CachePolicy::default(),
            generation: Mutex::new(Ok(Some(0))),
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
    fn is_cacheable(&self) -> bool {
        !matches!(self.policy, CachePolicy::Disabled)
    }
    fn negative_cache_generation(&self) -> VfsResult<Option<u64>> {
        if matches!(self.policy, CachePolicy::PositiveOnly) {
            Ok(None)
        } else {
            *self.generation.lock()
        }
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
        if let Ok(Some(generation)) = &mut *self.generation.lock() {
            *generation += 1;
        }
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
