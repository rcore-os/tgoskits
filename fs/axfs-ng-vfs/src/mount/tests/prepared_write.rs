//! Dependency descriptions are fixtures; all write ownership is real VFS state.

use super::*;
use crate::PreparedWrite;

struct WriteLayer {
    this: Weak<Self>,
    depth: u32,
    roots: Vec<Location>,
    operations: Option<Arc<OperationProbe>>,
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
    use crate::{
        MetadataChange, MutationCredentials, MutationRetire, NamespaceAdmissions, TimeChange,
    };

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
    let retired = MutationRetire::new();
    let admissions = NamespaceAdmissions::acquire(&write).unwrap();
    let admission = admissions.for_location(&root).unwrap();
    let dropped = Arc::new(core::sync::atomic::AtomicUsize::new(0));
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
    let completion = admission
        .change_metadata(
            &unsupported,
            MetadataChange::Chown {
                uid: None,
                gid: None,
            },
            &credentials,
            &write,
            &retired,
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        completion.outcome().state(),
        crate::NamespaceState::Unchanged
    );
    assert!(completion.outcome().observation().is_none());
    drop(unsupported);

    let backend = root.entry().downcast::<WriteLayerRoot>().unwrap();
    let alias_mount = Mountpoint::new_root(&Filesystem::new(backend.filesystem.clone()));
    let uncovered = Location::new(alias_mount, target().entry().clone());
    assert!(
        admission
            .change_metadata(&uncovered, empty, &credentials, &write, &retired)
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        admission.change_metadata(
            &uncovered,
            MetadataChange::Chmod(NodePermission::default()),
            &credentials,
            &write,
            &retired
        ),
        Err(VfsError::CrossesDevices)
    ));
    drop(uncovered);
    assert_eq!(dropped.load(Ordering::Relaxed), 0);
    assert_eq!(probe.metadata_reads.load(Ordering::Relaxed), 0);
    let mut deferred = admissions
        .pressure(vec![completion])
        .unwrap_or_else(|_| panic!("fixed metadata owner"));
    drop(admissions);
    assert_eq!(dropped.load(Ordering::Relaxed), 0);
    drop(retired);
    assert_eq!(dropped.load(Ordering::Relaxed), 2);
    assert_eq!(
        deferred.pop().unwrap().finish().into_result(),
        Err(VfsError::OperationNotSupported)
    );
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
    let filesystem = Filesystem::new(Arc::new_cyclic(|this| WriteLayer {
        this: this.clone(),
        depth,
        roots,
        operations,
    }));
    Mountpoint::new_root(&filesystem).root_location()
}

#[test]
fn namespace_admissions_order_real_receivers_once_and_release_partial_acquisitions() {
    let probe = Arc::new(OperationProbe::default());
    let first = probed_layer(0, vec![], Some(probe.clone()));
    let second = probed_layer(0, vec![], Some(probe.clone()));
    let address = |location: &Location| {
        location.filesystem() as *const dyn FilesystemOps as *const () as usize
    };
    // Reverse data-address order, including two exact mounts of one FS.
    let (higher, lower) = if address(&first) > address(&second) {
        (first, second)
    } else {
        (second, first)
    };
    let host = Mountpoint::new_root(&mock_filesystem());
    let target = Location::new(
        host.clone(),
        make_child_dir_entry(Some(host.root.clone()), "alias"),
    );
    let alias = target.bind_mount(&lower, false).unwrap().root_location();
    let visible = probed_layer(1, vec![higher, alias, lower], Some(probe.clone()));
    let write = PreparedWrite::acquire(&visible).unwrap();
    let admissions = crate::NamespaceAdmissions::acquire(&write).unwrap();
    let entered: Vec<_> = probe
        .events
        .lock()
        .unwrap()
        .iter()
        .filter_map(|(owner, enter)| enter.then_some(*owner))
        .collect();
    assert_eq!(
        entered.len(),
        3,
        "duplicate backing roots entered the FS twice"
    );
    assert!(entered.windows(2).all(|pair| pair[0] < pair[1]));
    drop(admissions);
    let events = core::mem::take(&mut *probe.events.lock().unwrap());
    for owner in &entered {
        assert_eq!(
            events
                .iter()
                .filter(|event| **event == (*owner, false))
                .count(),
            1
        );
    }

    // Fail the last acquisition: every earlier guard must be gone when the
    // factory returns the original error, not held in a partial preparation.
    probe
        .fail_owner
        .store(*entered.last().unwrap(), Ordering::Relaxed);
    assert!(matches!(
        crate::NamespaceAdmissions::acquire(&write),
        Err(VfsError::Io)
    ));
    let events = probe.events.lock().unwrap();
    assert_eq!(events.len(), 4);
    for owner in &entered[..2] {
        assert_eq!(
            events
                .iter()
                .filter(|event| **event == (*owner, true))
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| **event == (*owner, false))
                .count(),
            1
        );
    }
}

#[test]
fn prepared_write_retains_exact_mounts_across_dependencies_and_releases_failed_preparations() {
    let leaf = layer(0, vec![]);
    let host = Mountpoint::new_root(&mock_filesystem());
    let bind = |name| {
        let target = Location::new(
            host.clone(),
            make_child_dir_entry(Some(host.root.clone()), name),
        );
        target.bind_mount(&leaf, false).unwrap().root_location()
    };
    let first = bind("first");
    let second = bind("second");
    let uncovered = bind("uncovered");
    let middle = layer(1, vec![first.clone(), second.clone()]);
    let visible = layer(2, vec![middle.clone(), first.clone()]);
    let held = [&visible, &middle, &first, &second];

    let prepared = PreparedWrite::acquire(&visible).unwrap();
    for location in held {
        prepared
            .write_for(location)
            .unwrap()
            .check_location(location)
            .unwrap();
        // Counts come from real admission, not from the dependency fixture.
        // The repeated first root must not own a second lease.
        assert_eq!(location.mountpoint().writes.lock().writers, 1);
        assert_eq!(
            location.mountpoint().set_readonly(true),
            Err(VfsError::ResourceBusy)
        );
        assert!(!location.is_readonly());
    }
    assert_eq!(leaf.mountpoint().filesystem_state.writes.lock().writers, 2);
    assert_eq!(
        uncovered.mountpoint().set_filesystem_readonly(true),
        Err(VfsError::ResourceBusy)
    );
    assert_eq!(
        prepared.write_for(&uncovered).unwrap_err(),
        VfsError::CrossesDevices
    );
    assert_eq!(
        prepared.write_for(&first).unwrap().check_location(&second),
        Err(VfsError::CrossesDevices)
    );
    // The unprepared bind has no local writer, despite sharing the filesystem.
    uncovered.mountpoint().set_readonly(true).unwrap();
    assert_eq!(
        PreparedWrite::acquire(&uncovered).unwrap_err(),
        VfsError::ReadOnlyFilesystem
    );
    drop(prepared);
    for location in held {
        location.mountpoint().set_readonly(true).unwrap();
    }
    uncovered
        .mountpoint()
        .set_filesystem_readonly(true)
        .unwrap();

    for blocked in [&middle, &second] {
        for location in held {
            location
                .mountpoint()
                .set_filesystem_readonly(false)
                .unwrap();
            location.mountpoint().set_readonly(false).unwrap();
        }
        blocked.mountpoint().set_readonly(true).unwrap();
        assert_eq!(
            PreparedWrite::acquire(&visible).unwrap_err(),
            VfsError::ReadOnlyFilesystem
        );
        // Transitions succeed only if all earlier recursive admissions unwound.
        for location in held {
            location.mountpoint().set_readonly(true).unwrap();
            location.mountpoint().set_filesystem_readonly(true).unwrap();
        }
    }

    let nondecreasing = layer(1, vec![layer(1, vec![])]);
    for invalid in [layer(3, vec![leaf]), nondecreasing] {
        assert_eq!(
            PreparedWrite::acquire(&invalid).unwrap_err(),
            VfsError::InvalidInput
        );
        invalid.mountpoint().set_readonly(true).unwrap();
        invalid.mountpoint().set_filesystem_readonly(true).unwrap();
    }
}
