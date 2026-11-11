//! Deterministic backend interleavings exercise the actual DirNode cache.

use alloc::{boxed::Box, string::String, sync::Arc, vec};
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

#[test]
fn completion_pressure_preserves_typed_results_and_mismatched_owners() {
    // The fixture supplies receivers and completion work, not a replacement
    // admission/completion state machine or a metadata mutation algorithm.
    struct ForeignFilesystem(DirEntry);
    impl FilesystemOps for ForeignFilesystem {
        fn name(&self) -> &str {
            "foreign-completion-owner"
        }
        fn root_dir(&self) -> DirEntry {
            self.0.clone()
        }
        fn stat(&self) -> VfsResult<StatFs> {
            Err(VfsError::Unsupported)
        }
    }
    struct FinishProbe {
        calls: Arc<AtomicUsize>,
        _owner: Arc<()>,
    }
    impl NamespaceFinish for FinishProbe {
        fn finish(self: Box<Self>) -> VfsResult<()> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Err(VfsError::InvalidInput)
        }
    }
    struct PressureProbe {
        calls: Arc<AtomicUsize>,
        finish: FinishProbe,
        error: Option<VfsError>,
    }
    impl<T> NamespacePressure<T> for PressureProbe {
        fn pressure(
            self: Box<Self>,
            _outcome: &mut NamespaceOutcome<T>,
        ) -> NamespacePressureResult {
            self.calls.fetch_add(1, Ordering::Relaxed);
            NamespacePressureResult::new(
                self.error.map_or(Ok(()), Err),
                Some(Box::new(self.finish)),
            )
        }
    }

    let (directory, backend) = fixture();
    // ScriptedDirectory reports a shared zero-sized owner. Use a distinct
    // receiver, not another mount of that same owner, for the mismatch.
    let foreign = ForeignFilesystem(directory.0.entry().clone());
    let write = crate::PreparedWrite::acquire(&directory.0).unwrap();
    let admissions = crate::NamespaceAdmissions::acquire(&write).unwrap();
    let retired = MutationRetire::new();
    let pressure_calls = Arc::new(AtomicUsize::new(0));
    let finish_calls = Arc::new(AtomicUsize::new(0));
    let owner = Arc::new(());
    let work = || {
        Some(Box::new(PressureProbe {
            calls: pressure_calls.clone(),
            finish: FinishProbe {
                calls: finish_calls.clone(),
                _owner: owner.clone(),
            },
            error: Some(VfsError::Io),
        }))
    };
    let namespace: NamespaceCompletion = NamespaceCompletion::new(
        NamespaceOutcome::unchanged(Ok(())),
        work().map(|work| work as Box<dyn NamespacePressure>),
    )
    .bind(directory.0.filesystem());
    let snapshot = backend.metadata().unwrap();
    let metadata = NamespaceCompletion::new(
        // The same errno as the Retry marker is still a real operation error
        // when the state is Applied; later failures must not replace it.
        NamespaceOutcome::applied(snapshot.clone(), Err(VfsError::WouldBlock)),
        work().map(|work| work as Box<dyn NamespacePressure<Metadata>>),
    )
    .bind(directory.0.filesystem());
    let namespace = admissions.pressure(vec![namespace]).unwrap_or_else(|_| {
        panic!("the default namespace completion must match its real receiver")
    });
    let metadata = admissions
        .pressure(vec![metadata])
        .unwrap_or_else(|_| panic!("the metadata completion must match its real receiver"));

    // A later mismatch returns the entire typed set, without consuming even
    // its first (matching) pressure/finish owner or the earlier deferred sets.
    let rejected = vec![
        NamespaceCompletion::new(
            NamespaceOutcome::unchanged_observed(snapshot.clone(), Ok(())),
            work().map(|work| work as Box<dyn NamespacePressure<Metadata>>),
        )
        .bind(directory.0.filesystem()),
        NamespaceCompletion::new(
            NamespaceOutcome::uncertain(None, Err(VfsError::Io)),
            work().map(|work| work as Box<dyn NamespacePressure<Metadata>>),
        )
        .bind(&foreign),
    ];
    let rejected = match admissions.pressure(rejected) {
        Ok(_) => panic!("a foreign owner must reject the whole set"),
        Err(rejected) => rejected,
    };
    assert_eq!(rejected.len(), 2);
    assert!(rejected[0].admitted_by(&admissions));
    assert!(!rejected[1].admitted_by(&admissions));
    assert_eq!(rejected[0].outcome().state(), NamespaceState::Unchanged);
    assert_eq!(
        rejected[0].outcome().observation().unwrap().inode,
        snapshot.inode
    );
    assert_eq!(rejected[1].outcome().state(), NamespaceState::Uncertain);
    assert!(rejected[1].outcome().observation().is_none());
    assert_eq!(pressure_calls.load(Ordering::Relaxed), 2);
    assert_eq!(finish_calls.load(Ordering::Relaxed), 0);
    assert_eq!(Arc::strong_count(&owner), 5);

    drop(admissions);
    drop(retired);
    let mut namespace = namespace.into_iter();
    let namespace = namespace.next().unwrap().finish();
    assert_eq!(namespace.state(), NamespaceState::Unchanged);
    assert!(namespace.observation().is_none());
    assert_eq!(namespace.into_result(), Err(VfsError::Io));
    let mut metadata = metadata.into_iter();
    let metadata: NamespaceOutcome<Metadata> = metadata.next().unwrap().finish();
    assert_eq!(metadata.state(), NamespaceState::Applied);
    let observed = metadata.observation().unwrap();
    assert_eq!(observed.inode, snapshot.inode);
    assert_eq!(observed.mode.bits(), snapshot.mode.bits());
    assert_eq!(observed.uid, snapshot.uid);
    assert_eq!(metadata.into_result(), Err(VfsError::WouldBlock));
    assert_eq!(finish_calls.load(Ordering::Relaxed), 2);
    assert_eq!(Arc::strong_count(&owner), 3);
    // Abandoning mismatches releases responsibility, never runs completion.
    drop(rejected);
    assert_eq!(Arc::strong_count(&owner), 1);
    assert_eq!(pressure_calls.load(Ordering::Relaxed), 2);
    assert_eq!(finish_calls.load(Ordering::Relaxed), 2);

    // Retry's WouldBlock is control flow, not the first real failure. Exercise
    // both finish-only failure and an earlier pressure failure through the
    // actual typed completion phases, after releasing every admission.
    for (pressure_error, expected_error) in [
        (None, VfsError::InvalidInput),
        (Some(VfsError::Io), VfsError::Io),
    ] {
        let retired = MutationRetire::new();
        retired.retire(owner.clone());
        let admissions = crate::NamespaceAdmissions::acquire(&write).unwrap();
        let retry: NamespaceCompletion<Metadata> = NamespaceCompletion::new(
            NamespaceOutcome::retry(),
            Some(Box::new(PressureProbe {
                calls: pressure_calls.clone(),
                finish: FinishProbe {
                    calls: finish_calls.clone(),
                    _owner: owner.clone(),
                },
                error: pressure_error,
            })),
        )
        .bind(directory.0.filesystem());
        let mut remaining = admissions
            .pressure(vec![retry])
            .unwrap_or_else(|_| panic!("retry completion must match its real owner"));
        drop(admissions);
        drop(retired);
        let outcome = remaining.pop().unwrap().finish();
        assert_eq!(
            outcome.state(),
            NamespaceState::Unchanged,
            "a real completion failure must stop Retry"
        );
        assert!(outcome.observation().is_none());
        assert_eq!(outcome.into_result(), Err(expected_error));
        assert_eq!(Arc::strong_count(&owner), 1);
    }
    assert_eq!(pressure_calls.load(Ordering::Relaxed), 4);
    assert_eq!(finish_calls.load(Ordering::Relaxed), 4);

    // A composite owner needs each new phase error before merging. Neither a
    // successful Applied result nor its old error may hide pressure or finish.
    // This exercises the real phase bridge, not a substitute scope algorithm.
    for original in [Ok(()), Err(VfsError::WouldBlock)] {
        let admissions = crate::NamespaceAdmissions::acquire(&write).unwrap();
        let completion = NamespaceCompletion::new(
            NamespaceOutcome::applied(snapshot.clone(), original),
            work().map(|work| work as Box<dyn NamespacePressure<Metadata>>),
        )
        .bind(directory.0.filesystem());
        assert!(completion.admitted_by(&admissions));
        let (deferred, pressure) = completion.pressure_step();
        assert_eq!(pressure, Err(VfsError::Io));
        assert_eq!(
            finish_calls.load(Ordering::Relaxed) + 1,
            pressure_calls.load(Ordering::Relaxed)
        );
        drop(admissions);
        let (outcome, finish) = deferred.finish_step();
        assert_eq!(finish, Err(VfsError::InvalidInput));
        assert_eq!(outcome.state(), NamespaceState::Applied);
        assert_eq!(outcome.observation().unwrap().inode, snapshot.inode);
        assert_eq!(outcome.into_result(), original);
        assert_eq!(Arc::strong_count(&owner), 1);
    }
    assert_eq!(pressure_calls.load(Ordering::Relaxed), 6);
    assert_eq!(finish_calls.load(Ordering::Relaxed), 6);
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
        Ok(MutationObservation::Create {
            target,
            owner_policy: CreateOwnerPolicy::ParentSetgid,
        })
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
