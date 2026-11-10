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
        let directory = TestDirectory::new(backend.clone());
        assert_missing(&directory, "missing");
        assert_missing(&directory, "missing");
        assert_eq!(backend.lookups.load(Ordering::Relaxed), 2);
    }
}

#[test]
fn mixed_directory_revalidates_only_dynamic_children() {
    let backend = Arc::new(ScriptedDirectory {
        policy: CachePolicy::Mixed,
        ..Default::default()
    });
    let directory = TestDirectory::new(backend.clone());
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
            0,
            &crate::MutationCredentials {
                fsuid: 0,
                fsgid: 0,
                ..crate::MutationCredentials::root()
            }
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
    let alias = TestDirectory::new(backend);
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
    let alias = TestDirectory::new(backend.clone());
    *backend.after_lookup.lock() = Some(Box::new(move || {
        create_directory(&alias, "created");
    }));
    assert_missing(&directory, "created");
    assert!(directory.lookup("created").is_ok());
}

fn fixture() -> (Arc<TestDirectory>, Arc<ScriptedDirectory>) {
    let backend = Arc::new(ScriptedDirectory::default());
    (Arc::new(TestDirectory::new(backend.clone())), backend)
}

fn create_directory(directory: &TestDirectory, name: &str) -> DirEntry {
    directory
        .create(
            name,
            NodeType::Directory,
            NodePermission::default(),
            0,
            0,
            &crate::MutationCredentials {
                fsuid: 0,
                fsgid: 0,
                ..crate::MutationCredentials::root()
            },
        )
        .unwrap()
}

struct TestDirectory(crate::Location);

impl TestDirectory {
    fn new(backend: Arc<ScriptedDirectory>) -> Self {
        struct Root(DirEntry);
        impl FilesystemOps for Root {
            fn name(&self) -> &str {
                "scripted-directory"
            }
            fn root_dir(&self) -> DirEntry {
                self.0.clone()
            }
            fn stat(&self) -> VfsResult<StatFs> {
                Err(VfsError::Unsupported)
            }
        }
        let root = DirEntry::new_dir(|_| DirNode::new(backend), Reference::root());
        let fs = crate::Filesystem::new(Arc::new(Root(root)));
        Self(crate::Mountpoint::new_root(&fs).root_location())
    }

    fn create(
        &self,
        name: &str,
        kind: NodeType,
        mode: NodePermission,
        uid: u32,
        gid: u32,
        credentials: &crate::MutationCredentials<'_>,
    ) -> VfsResult<DirEntry> {
        self.0
            .create(name, kind, mode, uid, gid, credentials)
            .map(|entry| entry.entry().clone())
    }
}

impl core::ops::Deref for TestDirectory {
    type Target = DirNode;
    fn deref(&self) -> &Self::Target {
        self.0.entry().as_dir().unwrap()
    }
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
    Mixed,
}

struct ScriptedDirectory {
    mutation: std::sync::Mutex<()>,
    entries: RawSpinLock<HashMap<String, DirEntry>>,
    lookups: AtomicUsize,
    lookup_error: RawSpinLock<VfsError>,
    after_lookup: RawSpinLock<Option<Box<dyn FnOnce() + Send>>>,
    fail_after_create: AtomicBool,
    policy: CachePolicy,
    generation: RawSpinLock<VfsResult<Option<u64>>>,
}

impl Default for ScriptedDirectory {
    fn default() -> Self {
        Self {
            mutation: std::sync::Mutex::new(()),
            entries: RawSpinLock::new(HashMap::new()),
            lookups: AtomicUsize::new(0),
            lookup_error: RawSpinLock::new(VfsError::NotFound),
            after_lookup: RawSpinLock::new(None),
            fail_after_create: AtomicBool::new(false),
            policy: CachePolicy::default(),
            generation: RawSpinLock::new(Ok(Some(0))),
        }
    }
}

impl NodeOps for ScriptedDirectory {
    fn inode(&self) -> u64 {
        1
    }
    fn metadata(&self) -> VfsResult<Metadata> {
        Ok(Metadata {
            inode: 1,
            device: 0,
            nlink: 1,
            mode: NodePermission::default(),
            node_type: NodeType::Directory,
            uid: 0,
            gid: 0,
            size: 0,
            block_size: 4096,
            blocks: 0,
            rdev: crate::DeviceId::default(),
            atime: core::time::Duration::ZERO,
            mtime: core::time::Duration::ZERO,
            ctime: core::time::Duration::ZERO,
        })
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
    fn is_cacheable_child(&self, name: &str) -> bool {
        self.is_cacheable()
            && !(matches!(self.policy, CachePolicy::Mixed) && name.starts_with("volatile"))
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

    fn read_dir(&self, _: DirectoryCursor, _: &mut dyn DirEntrySink) -> VfsResult<usize> {
        Err(VfsError::Unsupported)
    }

    fn mutate_namespace(
        &self,
        command: &NamespaceMutation<'_>,
        retired: &MutationRetire,
    ) -> NamespaceOutcome<NamespaceEffect> {
        let mut attempt = ScriptedAttempt {
            backend: self,
            _gate: self.mutation.lock().unwrap(),
        };
        command.execute(&mut attempt, retired)
    }
}
impl ScriptedDirectory {
    fn create_locked(
        &self,
        name: &str,
        _: NodeType,
        _: NodePermission,
        _: u32,
        _: u32,
    ) -> NamespaceOutcome<DirEntry> {
        let entry = DirEntry::new_dir(
            |_| DirNode::new(Arc::new(Self::default())),
            Reference::root(),
        );
        self.entries.lock().insert(name.into(), entry.clone());
        if let Ok(Some(generation)) = &mut *self.generation.lock() {
            *generation += 1;
        }
        let completion = if self.fail_after_create.load(Ordering::Relaxed) {
            Err(VfsError::Io)
        } else {
            Ok(())
        };
        NamespaceOutcome::applied(entry, completion)
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

struct ScriptedAttempt<'a> {
    _gate: std::sync::MutexGuard<'a, ()>,
    backend: &'a ScriptedDirectory,
}

impl LockedAttempt for ScriptedAttempt<'_> {
    fn parents(&mut self) -> VfsResult<(Metadata, Metadata)> {
        let parent = self.backend.metadata()?;
        Ok((parent.clone(), parent))
    }
    fn observe(
        &mut self,
        command: &NamespaceMutation<'_>,
        retired: &MutationRetire,
    ) -> VfsResult<MutationObservation> {
        let NamespaceMutation::Create(request) = command else {
            return Err(VfsError::Unsupported);
        };
        let candidate = self.backend.entries.lock().get(request.name()).cloned();
        let target = if let Some(entry) = candidate {
            retired.retire(entry.clone());
            Some(RenameNode {
                metadata: entry.metadata()?,
                entry,
                canonical_name: request.name().into(),
                cacheable: self.backend.is_cacheable_child(request.name()),
                empty: false,
            })
        } else {
            None
        };
        Ok(MutationObservation::Create(target))
    }
    fn is_ancestor(&mut self, _: &NamespaceNode, _: u64) -> VfsResult<bool> {
        Err(VfsError::Unsupported)
    }
    fn commit(
        &mut self,
        authorized: &AuthorizedMutation<'_>,
        retired: &MutationRetire,
    ) -> NamespaceOutcome<NamespaceEffect> {
        let AuthorizedMutation::Create(authorized) = authorized else {
            return NamespaceOutcome::unchanged(Err(VfsError::Unsupported));
        };
        let request = authorized.request();
        let (uid, gid) = authorized.owner();
        self.backend
            .create_locked(
                request.name(),
                request.attributes().content.node_type(),
                authorized.permission(),
                uid,
                gid,
            )
            .map(|entry| {
                retired.retire(entry.clone());
                NamespaceEffect::Create(CreateEffect {
                    selected: NamespaceEntry {
                        entry,
                        canonical_name: request.name().into(),
                        cacheable: self.backend.is_cacheable_child(request.name()),
                    },
                    created: true,
                })
            })
    }
}
