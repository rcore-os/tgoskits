//! Dependency descriptions are fixtures; all write ownership is real VFS state.

use super::*;
use crate::PreparedWrite;

mod read_backing;

struct WriteLayer {
    this: Weak<Self>,
    depth: u32,
    read_roots: Vec<Location>,
    roots: Vec<Location>,
    operations: Option<Arc<OperationProbe>>,
    lookup_response: std::sync::Mutex<Option<crate::NamespaceRead>>,
}

#[derive(Default)]
struct OperationProbe {
    events: std::sync::Mutex<Vec<(usize, bool)>>,
    fail_owner: core::sync::atomic::AtomicUsize,
    metadata_reads: core::sync::atomic::AtomicUsize,
}

struct ProbedOperation<'a> {
    owner: usize,
    probe: &'a OperationProbe,
}

impl Drop for ProbedOperation<'_> {
    fn drop(&mut self) {
        self.probe.events.lock().unwrap().push((self.owner, false));
    }
}

struct WriteLayerRoot {
    filesystem: Arc<WriteLayer>,
    node: MockNode,
}

impl FilesystemOps for WriteLayer {
    fn query_namespace(
        &self,
        _admission: &crate::NamespaceAdmission<'_>,
        query: crate::NamespaceQuery<'_>,
        _retired: &crate::MutationRetire,
    ) -> VfsResult<crate::NamespaceRead> {
        // Supply only the response; VFS must do validation and retirement.
        if !matches!(query, crate::NamespaceQuery::Lookup { .. }) {
            return Err(VfsError::OperationNotSupported);
        }
        self.lookup_response
            .lock()
            .unwrap()
            .take()
            .ok_or(VfsError::OperationNotSupported)
    }

    fn enter_namespace(&self) -> VfsResult<Box<dyn crate::NamespaceOperation + '_>> {
        let Some(probe) = &self.operations else {
            return Ok(Box::new(()));
        };
        let owner = core::ptr::from_ref(self) as usize;
        if probe.fail_owner.load(Ordering::Relaxed) == owner {
            return Err(VfsError::Io);
        }
        probe.events.lock().unwrap().push((owner, true));
        Ok(Box::new(ProbedOperation { owner, probe }))
    }

    fn name(&self) -> &str {
        "write-layer-fixture"
    }

    fn root_dir(&self) -> DirEntry {
        let node = Arc::new(WriteLayerRoot {
            filesystem: self.this.upgrade().unwrap(),
            node: MockNode,
        });
        DirEntry::new_dir(|_| DirNode::new(node), Reference::root())
    }

    fn stack_depth(&self) -> u32 {
        self.depth
    }

    fn write_backing_roots(&self) -> &[Location] {
        &self.roots
    }

    fn read_backing_roots(&self) -> &[Location] {
        &self.read_roots
    }

    fn stat(&self) -> VfsResult<StatFs> {
        MOCK_FS.stat()
    }
}

#[inherit_methods(from = "self.node")]
impl NodeOps for WriteLayerRoot {
    fn inode(&self) -> u64;
    fn metadata(&self) -> VfsResult<Metadata> {
        if let Some(probe) = &self.filesystem.operations {
            probe.metadata_reads.fetch_add(1, Ordering::Relaxed);
        }
        self.node.metadata()
    }
    fn update_metadata(&self, update: MetadataUpdate) -> VfsResult<()>;
    fn sync(&self, data_only: bool) -> VfsResult<()>;

    fn filesystem(&self) -> &dyn FilesystemOps {
        self.filesystem.as_ref()
    }

    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }
}

#[test]
fn metadata_dispatch_retains_failed_targets_and_empty_times_need_no_rights() {
    use crate::{MetadataChange, MutationCredentials, NamespaceOperationScope, TimeChange};

    struct RetiredTarget {
        probe: Arc<OperationProbe>,
        dropped: Arc<core::sync::atomic::AtomicUsize>,
    }
    impl Drop for RetiredTarget {
        fn drop(&mut self) {
            let events = self.probe.events.lock().unwrap();
            assert_eq!(
                events.iter().filter(|(_, enter)| *enter).count(),
                events.iter().filter(|(_, enter)| !*enter).count(),
                "last target owner must not be destroyed under operation admission"
            );
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
    struct RetirementBoundary {
        probe: Arc<OperationProbe>,
        dropped: Arc<core::sync::atomic::AtomicUsize>,
    }
    impl Drop for RetirementBoundary {
        fn drop(&mut self) {
            let events = self.probe.events.lock().unwrap();
            assert_eq!(
                events.iter().filter(|(_, enter)| *enter).count(),
                events.iter().filter(|(_, enter)| !*enter).count()
            );
            assert_eq!(self.dropped.load(Ordering::Relaxed), 0);
        }
    }
    let probe = Arc::new(OperationProbe::default());
    let root = probed_layer(0, vec![], Some(probe.clone()));
    let credentials = MutationCredentials::root();
    let empty = MetadataChange::Times {
        atime: TimeChange::Keep,
        mtime: TimeChange::Keep,
    };
    root.mountpoint().set_readonly(true).unwrap();
    root.change_metadata(empty, &credentials).unwrap();
    assert!(probe.events.lock().unwrap().is_empty());
    assert_eq!(probe.metadata_reads.load(Ordering::Relaxed), 0);
    root.mountpoint().set_readonly(false).unwrap();

    let write = PreparedWrite::acquire(&root).unwrap();
    let scope = NamespaceOperationScope::<Metadata>::acquire(&write).unwrap();
    let context = scope.context();
    let dropped = Arc::new(core::sync::atomic::AtomicUsize::new(0));
    context.retired().retire(RetirementBoundary {
        probe: probe.clone(),
        dropped: dropped.clone(),
    });
    let target = || {
        // A real, uncached VFS alias with no mount-root reference to it.
        let entry = root.filesystem().root_dir();
        entry.user_data().insert(RetiredTarget {
            probe: probe.clone(),
            dropped: dropped.clone(),
        });
        Location::new(root.mountpoint().clone(), entry)
    };
    let unsupported = target();
    let completion = scope
        .change_metadata(
            &context,
            &unsupported,
            MetadataChange::Chown {
                uid: None,
                gid: None,
            },
            &credentials,
        )
        .unwrap()
        .unwrap();
    assert_eq!(completion.state(), crate::NamespaceState::Unchanged);
    assert!(completion.observation().is_none());
    drop(unsupported);

    let backend = root.entry().downcast::<WriteLayerRoot>().unwrap();
    let alias_mount = Mountpoint::new_root(&Filesystem::new(backend.filesystem.clone()));
    let uncovered = Location::new(alias_mount, target().entry().clone());
    assert!(
        context
            .change_metadata(&uncovered, empty, &credentials)
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        context.change_metadata(
            &uncovered,
            MetadataChange::Chmod(NodePermission::default()),
            &credentials
        ),
        Err(VfsError::CrossesDevices)
    ));
    drop(uncovered);
    assert_eq!(dropped.load(Ordering::Relaxed), 0);
    assert_eq!(probe.metadata_reads.load(Ordering::Relaxed), 0);
    let outcome = scope
        .finish()
        .unwrap_or_else(|_| panic!("fixed metadata owner"));
    assert_eq!(dropped.load(Ordering::Relaxed), 2);
    assert_eq!(outcome.into_result(), Err(VfsError::OperationNotSupported));
}

#[inherit_methods(from = "self.node")]
impl DirNodeOps for WriteLayerRoot {
    fn read_dir(&self, cursor: DirectoryCursor, sink: &mut dyn DirEntrySink) -> VfsResult<usize>;
    fn lookup(&self, name: &str) -> VfsResult<DirEntry>;
    fn mutate_namespace(
        &self,
        command: &crate::NamespaceMutation<'_>,
        retired: &crate::MutationRetire,
    ) -> crate::NamespaceOutcome<crate::NamespaceEffect>;
}

fn layer(depth: u32, roots: Vec<Location>) -> Location {
    probed_layer(depth, roots, None)
}

fn probed_layer(
    depth: u32,
    roots: Vec<Location>,
    operations: Option<Arc<OperationProbe>>,
) -> Location {
    read_layer(depth, vec![], roots, operations)
}

fn read_layer(
    depth: u32,
    read_roots: Vec<Location>,
    roots: Vec<Location>,
    operations: Option<Arc<OperationProbe>>,
) -> Location {
    let filesystem = Filesystem::new(Arc::new_cyclic(|this| WriteLayer {
        this: this.clone(),
        depth,
        read_roots,
        roots,
        operations,
        lookup_response: std::sync::Mutex::new(None),
    }));
    Mountpoint::new_root(&filesystem).root_location()
}

#[test]
fn admitted_backing_lookup_validates_provider_results_and_retires_all_owners_after_exit() {
    use crate::{MutationRetire, NamespaceAdmissions, NamespaceRead};

    struct RetiredLookupOwner {
        name: &'static str,
        probe: Arc<OperationProbe>,
        dropped: Arc<std::sync::Mutex<Vec<&'static str>>>,
    }

    impl Drop for RetiredLookupOwner {
        fn drop(&mut self) {
            let events = self.probe.events.lock().unwrap();
            assert_eq!(
                events.iter().filter(|(_, enter)| *enter).count(),
                events.iter().filter(|(_, enter)| !*enter).count(),
                "last lookup owner was destroyed under operation admission"
            );
            self.dropped.lock().unwrap().push(self.name);
        }
    }

    let probe = Arc::new(OperationProbe::default());
    let dropped = Arc::new(std::sync::Mutex::new(Vec::new()));
    let marker = |name| RetiredLookupOwner {
        name,
        probe: probe.clone(),
        dropped: dropped.clone(),
    };
    let root = probed_layer(0, vec![], Some(probe.clone()));
    let provider = root
        .entry()
        .downcast::<WriteLayerRoot>()
        .unwrap()
        .filesystem
        .clone();
    let owner = core::ptr::from_ref(provider.as_ref()) as usize;
    let weak_provider = Arc::downgrade(&provider);
    let weak_root_mount = Arc::downgrade(root.mountpoint());

    // Same FS, different exact mount: PreparedWrite owns only root's mount.
    // The retired full parent must be what keeps this alias mount alive.
    let alias = Mountpoint::new_root(&Filesystem::new(provider.clone())).root_location();
    let weak_alias_mount = Arc::downgrade(alias.mountpoint());
    alias.user_data().insert(marker("parent"));

    let foreign_parent = layer(0, vec![]);
    let weak_foreign_mount = Arc::downgrade(foreign_parent.mountpoint());
    let weak_foreign_parent = Arc::downgrade(
        &foreign_parent
            .entry()
            .downcast::<WriteLayerRoot>()
            .unwrap()
            .filesystem,
    );
    foreign_parent.user_data().insert(marker("foreign-parent"));

    // The rejected candidate has no mount-root/cache owner after setup.
    let foreign = layer(0, vec![]);
    let weak_foreign_candidate = Arc::downgrade(
        &foreign
            .entry()
            .downcast::<WriteLayerRoot>()
            .unwrap()
            .filesystem,
    );
    let candidate = foreign.filesystem().root_dir();
    candidate.user_data().insert(marker("candidate"));
    drop(foreign);

    let write = PreparedWrite::acquire(&root).unwrap();
    let retired = MutationRetire::new();
    let admissions = NamespaceAdmissions::acquire(&write).unwrap();
    let admission = admissions.for_location(&root).unwrap();

    *provider.lookup_response.lock().unwrap() = Some(NamespaceRead::Entry(None));
    assert!(matches!(
        admission.lookup_backing(&foreign_parent, "bad/name", &retired),
        Err(VfsError::CrossesDevices)
    ));
    assert!(provider.lookup_response.lock().unwrap().is_some());
    assert!(matches!(
        admission.lookup_backing(&alias, "bad/name", &retired),
        Err(VfsError::InvalidInput)
    ));
    assert!(provider.lookup_response.lock().unwrap().is_some());
    assert!(matches!(
        admission.lookup_backing(&alias, "missing", &retired),
        Err(VfsError::NotFound)
    ));
    assert!(provider.lookup_response.lock().unwrap().is_none());

    *provider.lookup_response.lock().unwrap() = Some(NamespaceRead::Xattr(vec![1]));
    assert!(matches!(
        admission.lookup_backing(&alias, "wrong-shape", &retired),
        Err(VfsError::BadState)
    ));
    assert!(provider.lookup_response.lock().unwrap().is_none());

    *provider.lookup_response.lock().unwrap() = Some(NamespaceRead::Entry(Some(candidate)));
    assert!(matches!(
        admission.lookup_backing(&alias, "foreign-child", &retired),
        Err(VfsError::CrossesDevices)
    ));
    assert!(provider.lookup_response.lock().unwrap().is_none());

    let child = provider.root_dir();
    child.user_data().insert(marker("success"));
    let weak_child = child.downgrade();
    *provider.lookup_response.lock().unwrap() = Some(NamespaceRead::Entry(Some(child)));
    let selected = admission.lookup_backing(&alias, "child", &retired).unwrap();
    assert!(Arc::ptr_eq(selected.mountpoint(), alias.mountpoint()));
    assert!(selected.entry().ptr_eq(&weak_child.upgrade().unwrap()));
    drop(selected);
    drop(alias);
    drop(foreign_parent);
    drop(root);
    drop(provider);

    assert_eq!(
        *probe.events.lock().unwrap(),
        vec![(owner, true)],
        "lookup must use the existing admission without reentering"
    );
    assert!(dropped.lock().unwrap().is_empty());
    assert!(weak_alias_mount.upgrade().is_some());
    assert!(weak_foreign_mount.upgrade().is_some());
    assert!(weak_foreign_parent.upgrade().is_some());
    assert!(weak_foreign_candidate.upgrade().is_some());
    assert!(weak_child.upgrade().is_some());

    drop(admissions);
    assert_eq!(
        *probe.events.lock().unwrap(),
        vec![(owner, true), (owner, false)]
    );
    assert!(dropped.lock().unwrap().is_empty());
    assert!(weak_alias_mount.upgrade().is_some());
    assert!(weak_foreign_mount.upgrade().is_some());
    assert!(weak_foreign_parent.upgrade().is_some());
    assert!(weak_foreign_candidate.upgrade().is_some());
    assert!(weak_child.upgrade().is_some());

    drop(retired);
    let mut released = dropped.lock().unwrap().clone();
    released.sort_unstable();
    assert_eq!(
        released,
        vec!["candidate", "foreign-parent", "parent", "success"]
    );
    assert!(weak_alias_mount.upgrade().is_none());
    assert!(weak_foreign_mount.upgrade().is_none());
    assert!(weak_foreign_parent.upgrade().is_none());
    assert!(weak_foreign_candidate.upgrade().is_none());
    assert!(weak_child.upgrade().is_none());

    // PreparedWrite still owns the actual receiver, unlike the retired alias.
    assert!(weak_root_mount.upgrade().is_some());
    assert!(weak_provider.upgrade().is_some());
    drop(write);
    assert!(weak_root_mount.upgrade().is_none());
    assert!(weak_provider.upgrade().is_none());
}
