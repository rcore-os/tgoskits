use super::*;

fn adopt(root: &Arc<Mountpoint>) -> Arc<MountNamespace> {
    MountNamespace::from_tree(root.clone())
        .unwrap()
        .into_namespace()
}

fn directory(parent: &Arc<Mountpoint>, name: &str) -> Location {
    Location::new(
        parent.clone(),
        make_child_dir_entry(Some(parent.root.clone()), name),
    )
}

#[test]
fn namespace_anchor_stays_fixed_across_pivot_bind_and_owner_retirement() {
    let anchor = Mountpoint::new_root_with_source_readonly(&mock_filesystem(), "nullfs", true);
    let old = anchor.root_location().mount(&mock_filesystem()).unwrap();
    let new = directory(&old, "new").mount(&mock_filesystem()).unwrap();
    let namespace = adopt(&anchor);
    assert!(matches!(
        MountNamespace::from_tree(old.clone()),
        Err(VfsError::InvalidInput)
    ));
    assert!(matches!(
        MountNamespace::from_tree(anchor.clone()),
        Err(VfsError::InvalidInput)
    ));
    assert_eq!(
        anchor.pivot_mount(&new, &directory(&new, "old")),
        Err(VfsError::InvalidInput),
        "even the low-level pivot must not move an adopted anchor"
    );
    old.pivot_mount(&new, &directory(&new, "old")).unwrap();
    let (snapshot, retired) = namespace.snapshot_root();
    let snapshot = snapshot.unwrap();
    assert!(Arc::ptr_eq(namespace.root_mount(), &anchor));
    assert!(Arc::ptr_eq(snapshot.namespace(), &namespace));
    assert!(Arc::ptr_eq(snapshot.root().location().mountpoint(), &new));
    assert!(Arc::ptr_eq(old.location().unwrap().mountpoint(), &new));
    drop((snapshot, retired));

    // Preserve the existing same-point stack algorithm: old temporarily
    // covers next until explicitly detached, without changing the anchor.
    let next = directory(&new, "next").mount(&mock_filesystem()).unwrap();
    new.pivot_mount(&next, &next.root_location()).unwrap();
    let (covered, retired) = namespace.snapshot_root();
    assert!(Arc::ptr_eq(
        covered.unwrap().root().location().mountpoint(),
        &new
    ));
    drop(retired);
    new.detach().unwrap();
    let (visible, retired) = namespace.snapshot_root();
    assert!(Arc::ptr_eq(
        visible.unwrap().root().location().mountpoint(),
        &next
    ));
    drop(retired);

    // Binding an adopted root must not copy its role to the new node.
    let outside = Mountpoint::new_root(&mock_filesystem());
    let bound = directory(&outside, "bind")
        .bind_mount(&anchor.root_location(), true)
        .unwrap();
    bound.detach().unwrap();
    assert!(
        matches!(
            MountNamespace::from_tree(bound.clone()),
            Err(VfsError::InvalidInput)
        ),
        "a detached former child is not a fresh initialization root"
    );
    bound
        .attach_detached(&directory(&outside, "moved"))
        .unwrap();
    for child in bound.children() {
        child.detach().unwrap();
    }
    bound.detach().unwrap();

    // Recursive detach above already removed this former descendant too.
    assert!(old.location().is_none());
    drop(namespace);
    assert_eq!(
        anchor.attach_detached(&directory(&outside, "anchor")),
        Err(VfsError::InvalidInput),
        "the role belongs to the node, not a live namespace wrapper"
    );
    assert_eq!(
        anchor.move_to(&directory(&outside, "anchor")),
        Err(VfsError::InvalidInput)
    );
    assert!(matches!(
        MountNamespace::from_tree(anchor),
        Err(VfsError::InvalidInput)
    ));
    assert!(next.location().is_none());
    assert!(next.children().is_empty());
}

#[test]
fn namespace_clone_maps_stacked_mounts_and_shares_uses_until_outer_retirement() {
    let anchor = Mountpoint::new_root_with_source(&mock_filesystem(), "nullfs");
    let lower = anchor.root_location().mount(&mock_filesystem()).unwrap();
    let upper = lower.root_location().mount(&mock_filesystem()).unwrap();
    let namespace = adopt(&anchor);
    let root = PathHandle::acquire(upper.root_location()).unwrap();
    let cwd = root.rebind(directory(&upper, "cwd")).unwrap();
    let hidden = PathHandle::acquire(lower.root_location()).unwrap();
    let outside = Mountpoint::new_root(&mock_filesystem());
    let external = PathHandle::acquire(outside.root_location()).unwrap();
    let mut cloned = namespace.clone_namespace();
    assert!(Arc::ptr_eq(cloned.source_namespace(), &namespace));
    assert!(!Arc::ptr_eq(cloned.namespace(), &namespace));
    assert_eq!(cloned.namespace().walk_tree().len(), 3);
    let new_anchor = cloned.namespace().root_mount().clone();
    assert_eq!(new_anchor.source(), "nullfs");
    let mapped_root = cloned.map_path(&root).unwrap();
    let mapped_cwd = cloned.map_path(&cwd).unwrap();
    let mapped_hidden = cloned.map_path(&hidden).unwrap();
    let unmapped = cloned.map_path(&external).unwrap();
    let new_upper = mapped_root.location().mountpoint().clone();
    let new_lower = mapped_hidden.location().mountpoint().clone();
    assert!(!Arc::ptr_eq(&new_upper, &upper));
    assert!(Arc::ptr_eq(mapped_cwd.location().mountpoint(), &new_upper));
    assert!(mapped_cwd.location().entry().ptr_eq(cwd.location().entry()));
    assert!(
        mapped_hidden
            .location()
            .entry()
            .ptr_eq(hidden.location().entry())
    );
    assert!(unmapped.location().ptr_eq(external.location()));
    assert_eq!(new_upper.active_uses.lock().users, 1);
    assert_eq!(new_lower.active_uses.lock().users, 1);
    assert_eq!(outside.active_uses.lock().users, 1);

    let (created, retirement) = cloned.into_parts();
    let cloned_namespace = created.into_namespace();
    let (snapshot, root_retirement) = cloned_namespace.snapshot_root();
    assert!(Arc::ptr_eq(
        snapshot.unwrap().root().location().mountpoint(),
        &new_upper
    ));
    drop(root_retirement);
    let source_weak = Arc::downgrade(&namespace);
    drop((mapped_root, mapped_cwd, mapped_hidden, namespace));
    assert!(source_weak.upgrade().is_some());
    assert_eq!(
        new_upper.root_location().unmount(),
        Err(VfsError::ResourceBusy),
        "into_parts must transfer the mapper's last shared use, not release it"
    );
    drop(retirement);
    assert!(source_weak.upgrade().is_none());
    new_upper.root_location().unmount().unwrap();
    new_lower.root_location().unmount().unwrap();
    assert_eq!(
        new_anchor.attach_detached(&directory(&outside, "clone-anchor")),
        Err(VfsError::InvalidInput),
        "a full namespace clone establishes a fresh anchor role"
    );
    drop((external, unmapped));
    assert_eq!(outside.active_uses.lock().users, 0);
    drop((root, cwd, hidden));
    assert!(upper.location().is_none());
    assert!(lower.location().is_none());
    assert!(anchor.children().is_empty());
    assert!(lower.children().is_empty());
}

#[test]
fn namespace_root_success_and_failure_keep_last_owners_for_outer_retirement() {
    for fail in [false, true] {
        let outer = Arc::new(RawSpinLock::new(()));
        let retired = Arc::new(AtomicUsize::new(0));
        let gate = outer.clone();
        let count = retired.clone();
        let fs = Filesystem::new(Arc::new_cyclic(|self_ref| TopologyChangingFs {
            self_ref: self_ref.clone(),
            topology_change: Box::new(|| Ok(())),
            flushes: AtomicUsize::new(0),
            on_drop: Some(Box::new(move || {
                assert!(!current_thread_owns_mount_topology_guard());
                assert!(gate.try_lock().is_some(), "outer writer must be released");
                count.fetch_add(1, Ordering::Release);
            })),
        }));
        let root = Mountpoint::new_root(&fs);
        let namespace = adopt(&root);
        let namespace_weak = Arc::downgrade(&namespace);
        drop(fs);
        let writer = outer.lock();
        if fail {
            root.active_uses.lock().users = usize::MAX;
        }
        let (result, retirement) = namespace.snapshot_root();
        if fail {
            assert!(matches!(result, Err(VfsError::ValueOverflow)));
            assert_eq!(root.active_uses.lock().users, usize::MAX);
            root.active_uses.lock().users = 0;
        } else {
            let snapshot = result.unwrap();
            assert!(Arc::ptr_eq(snapshot.namespace(), &namespace));
            let (selected_namespace, path) = snapshot.into_parts();
            assert!(path.location().ptr_eq(&root.root_location()));
            drop((selected_namespace, path));
            assert_eq!(root.active_uses.lock().users, 1);
        }
        drop((root, namespace));
        assert!(namespace_weak.upgrade().is_some());
        assert_eq!(retired.load(Ordering::Acquire), 0);
        drop(writer);
        drop(retirement);
        assert!(namespace_weak.upgrade().is_none());
        assert_eq!(retired.load(Ordering::Acquire), 1);
    }
}

#[test]
fn namespace_clone_failure_and_into_parts_retain_last_filesystem_owner() {
    for fail in [false, true] {
        let outer = Arc::new(RawSpinLock::new(()));
        let retired = Arc::new(AtomicUsize::new(0));
        let gate = outer.clone();
        let count = retired.clone();
        let fs = Filesystem::new(Arc::new_cyclic(|self_ref| TopologyChangingFs {
            self_ref: self_ref.clone(),
            topology_change: Box::new(|| Ok(())),
            flushes: AtomicUsize::new(0),
            on_drop: Some(Box::new(move || {
                assert!(!current_thread_owns_mount_topology_guard());
                assert!(gate.try_lock().is_some(), "outer writer must be released");
                count.fetch_add(1, Ordering::Release);
            })),
        }));
        let namespace = adopt(&Mountpoint::new_root(&fs));
        let source = PathHandle::acquire(namespace.root_mount().root_location()).unwrap();
        drop(fs);
        let writer = outer.lock();
        let mut cloned = namespace.clone_namespace();
        if fail {
            cloned.namespace().root_mount().active_uses.lock().users = usize::MAX;
            assert!(matches!(
                cloned.map_path(&source),
                Err(VfsError::ValueOverflow)
            ));
            cloned.namespace().root_mount().active_uses.lock().users = 0;
        } else {
            drop(cloned.map_path(&source).unwrap());
            assert_eq!(cloned.namespace().root_mount().active_uses.lock().users, 1);
        }
        let (created, retirement) = cloned.into_parts();
        drop((namespace, source));
        assert_eq!(retired.load(Ordering::Acquire), 0);
        drop(writer);
        // A final Core may enter topology cleanup, so creation provenance
        // must leave the writer together with all other owning resources.
        drop(created);
        assert_eq!(retired.load(Ordering::Acquire), 0);
        drop(retirement);
        assert_eq!(retired.load(Ordering::Acquire), 1);
    }
}

#[derive(Debug)]
struct OutsideGateDrop {
    count: Arc<AtomicUsize>,
    outer: Arc<RawSpinLock<()>>,
}

impl Drop for OutsideGateDrop {
    fn drop(&mut self) {
        assert!(!current_thread_owns_mount_topology_guard());
        assert!(
            self.outer.try_lock().is_some(),
            "owned resources must retire outside the caller gate"
        );
        self.count.fetch_add(1, Ordering::Release);
    }
}

#[derive(Clone, Default)]
struct RetirementCounts {
    filesystem: Arc<AtomicUsize>,
    lease: Arc<AtomicUsize>,
    attachment: Arc<AtomicUsize>,
    flushes: Arc<AtomicUsize>,
}

impl RetirementCounts {
    fn probe(&self, count: &Arc<AtomicUsize>, outer: &Arc<RawSpinLock<()>>) -> OutsideGateDrop {
        OutsideGateDrop {
            count: count.clone(),
            outer: outer.clone(),
        }
    }

    fn filesystem(&self, outer: &Arc<RawSpinLock<()>>) -> Filesystem {
        Filesystem::new(Arc::new_cyclic(|self_ref| RetirementFilesystem {
            self_ref: self_ref.clone(),
            lease: RawSpinLock::new(Weak::new()),
            lease_drop: self.lease.clone(),
            outer: outer.clone(),
            flushes: self.flushes.clone(),
            _drop: self.probe(&self.filesystem, outer),
        }))
    }
}

/// A read-only backend fixture with real node ownership and a shared mount
/// lease. It models directory contents only, never namespace lifecycle state.
#[derive(Debug)]
struct RetirementFilesystem {
    self_ref: Weak<Self>,
    lease: RawSpinLock<Weak<RetirementLease>>,
    lease_drop: Arc<AtomicUsize>,
    outer: Arc<RawSpinLock<()>>,
    flushes: Arc<AtomicUsize>,
    _drop: OutsideGateDrop,
}

#[derive(Debug)]
struct RetirementLease {
    _filesystem: Arc<RetirementFilesystem>,
    _drop: OutsideGateDrop,
}

impl crate::FilesystemMountLease for RetirementLease {}

impl FilesystemOps for RetirementFilesystem {
    fn name(&self) -> &str {
        "namespace-retirement-fixture"
    }

    fn root_dir(&self) -> DirEntry {
        let filesystem = self.self_ref.upgrade().unwrap();
        DirEntry::new_dir(
            |_| DirNode::new(Arc::new(RetirementDirectory(filesystem))),
            Reference::root(),
        )
    }

    fn mount_lease(&self) -> Option<Arc<dyn crate::FilesystemMountLease>> {
        let mut slot = self.lease.lock();
        let lease = slot.upgrade().unwrap_or_else(|| {
            let lease = Arc::new(RetirementLease {
                _filesystem: self.self_ref.upgrade().unwrap(),
                _drop: OutsideGateDrop {
                    count: self.lease_drop.clone(),
                    outer: self.outer.clone(),
                },
            });
            *slot = Arc::downgrade(&lease);
            lease
        });
        Some(lease)
    }

    fn stat(&self) -> VfsResult<StatFs> {
        Err(VfsError::Unsupported)
    }

    fn flush(&self) -> VfsResult<()> {
        self.flushes.fetch_add(1, Ordering::Relaxed);
        Err(VfsError::Io)
    }
}

struct RetirementDirectory(Arc<RetirementFilesystem>);

impl NodeOps for RetirementDirectory {
    fn inode(&self) -> u64 {
        41
    }

    fn metadata(&self) -> VfsResult<Metadata> {
        Ok(mock_metadata(41, NodeType::Directory, 0))
    }

    fn update_metadata(&self, _: MetadataUpdate) -> VfsResult<()> {
        Err(VfsError::ReadOnlyFilesystem)
    }

    fn filesystem(&self) -> &dyn FilesystemOps {
        self.0.as_ref()
    }

    fn sync(&self, _: bool) -> VfsResult<()> {
        panic!("namespace retirement must not sync backend nodes")
    }

    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }
}

impl DirNodeOps for RetirementDirectory {
    fn read_dir(&self, cursor: DirectoryCursor, sink: &mut dyn DirEntrySink) -> VfsResult<usize> {
        Ok(usize::from(
            cursor.offset() == 0
                && sink.accept(
                    b"payload",
                    42,
                    NodeType::RegularFile,
                    DirectoryCursor::new(1),
                ),
        ))
    }

    fn lookup(&self, _: &str) -> VfsResult<DirEntry> {
        Err(VfsError::NotFound)
    }

    fn mutate_namespace(
        &self,
        _: &crate::NamespaceMutation<'_>,
        _: &crate::MutationRetire,
    ) -> crate::NamespaceOutcome<crate::NamespaceEffect> {
        crate::NamespaceOutcome::unchanged(Err(VfsError::ReadOnlyFilesystem))
    }
}

fn assert_payload(path: &Location) {
    let mut entries = Vec::new();
    path.read_dir(
        DirectoryCursor::START,
        &mut |name: &[u8], inode, kind, _| {
            entries.push((name.to_vec(), inode, kind));
            true
        },
    )
    .unwrap();
    assert_eq!(entries, [(b"payload".to_vec(), 42, NodeType::RegularFile)]);
}

#[derive(Clone, Copy, Debug)]
enum CloneExit {
    AbandonPreparation,
    DropCreated,
    DropConsumedCore,
}

#[test]
fn populated_namespace_clone_exit_reclaims_mounts_and_exclusive_backend() {
    for exit in [
        CloneExit::AbandonPreparation,
        CloneExit::DropCreated,
        CloneExit::DropConsumedCore,
    ] {
        let source_anchor = Mountpoint::new_root(&mock_filesystem());
        let source_cover = source_anchor
            .root_location()
            .mount(&mock_filesystem())
            .unwrap();
        let source_child = directory(&source_cover, "child")
            .mount(&mock_filesystem())
            .unwrap();
        let source = adopt(&source_anchor);
        let cloned = source.clone_namespace();
        let cloned_anchor = cloned.namespace().root_mount().clone();
        let cloned_cover = cloned_anchor.children().pop().unwrap();
        let cloned_child = cloned_cover.children().pop().unwrap();
        let outer = Arc::new(RawSpinLock::new(()));
        let counts = RetirementCounts::default();
        // This private branch is absent from the still-live source tree.
        let exclusive = directory(&cloned_child, "exclusive")
            .mount(&counts.filesystem(&outer))
            .unwrap();
        exclusive.set_lifetime_guard(Arc::new(counts.probe(&counts.attachment, &outer)));
        let weak_mounts: Vec<_> = [&cloned_anchor, &cloned_cover, &cloned_child, &exclusive]
            .into_iter()
            .map(Arc::downgrade)
            .collect();
        let path = matches!(exit, CloneExit::DropConsumedCore)
            .then(|| PathHandle::acquire(exclusive.root_location()).unwrap());
        drop((cloned_anchor, cloned_cover, cloned_child, exclusive));

        match exit {
            CloneExit::AbandonPreparation => drop(cloned),
            CloneExit::DropCreated | CloneExit::DropConsumedCore => {
                let writer = outer.lock();
                let (created, retirement) = cloned.into_parts();
                drop(writer);
                match exit {
                    CloneExit::DropCreated => drop((created, retirement)),
                    CloneExit::DropConsumedCore => {
                        let core = created.into_namespace();
                        drop((core, retirement));
                    }
                    CloneExit::AbandonPreparation => unreachable!(),
                }
            }
        }

        for mount in &weak_mounts[..3] {
            assert!(
                mount.upgrade().is_none(),
                "{exit:?}: populated clone must release actual mount allocations"
            );
        }
        assert_eq!(counts.attachment.load(Ordering::Acquire), 1, "{exit:?}");
        assert_eq!(counts.flushes.load(Ordering::Acquire), 0);
        if let Some(path) = path {
            let mount = path.location().mountpoint();
            assert!(mount.location().is_none());
            assert!(mount.children().is_empty());
            assert_eq!(mount.active_uses.lock().users, 1);
            assert!(!mount.active_uses.lock().normally_unmounted);
            assert!(matches!(
                path.location().parent_step().unwrap(),
                ParentStep::Root
            ));
            assert_payload(path.location());
            drop(path.rebind(path.location().clone()).unwrap());
            assert_eq!(counts.filesystem.load(Ordering::Acquire), 0);
            assert_eq!(counts.lease.load(Ordering::Acquire), 0);
            drop(path);
        }
        assert!(weak_mounts[3].upgrade().is_none(), "{exit:?}");
        assert_eq!(counts.filesystem.load(Ordering::Acquire), 1, "{exit:?}");
        assert_eq!(counts.lease.load(Ordering::Acquire), 1, "{exit:?}");
        assert!(source_cover.location().is_some());
        assert!(source_child.location().is_some());
        assert_eq!(source.walk_tree().len(), 3);
        // No manual detach/unmount is allowed to make clone reclamation pass.
        drop(source);
    }
}

#[test]
fn retired_paths_reject_topology_publication_without_unmounting_live_relatives() {
    let outer = Arc::new(RawSpinLock::new(()));
    let contents = RetirementCounts::default();
    let source_anchor = Mountpoint::new_root(&mock_filesystem());
    let shared = source_anchor
        .root_location()
        .mount(&contents.filesystem(&outer))
        .unwrap();
    let source = adopt(&source_anchor);
    shared.set_shared();

    let peer_anchor = Mountpoint::new_root(&mock_filesystem());
    let peer_namespace = adopt(&peer_anchor);
    let peer = directory(&peer_anchor, "peer")
        .bind_mount(&shared.root_location(), false)
        .unwrap();
    let slave_anchor = Mountpoint::new_root(&mock_filesystem());
    let slave_namespace = adopt(&slave_anchor);
    let slave = directory(&slave_anchor, "slave")
        .bind_mount(&shared.root_location(), false)
        .unwrap();
    slave.set_slave();
    let bind_anchor = Mountpoint::new_root(&mock_filesystem());
    let bind_namespace = adopt(&bind_anchor);
    let bound = directory(&bind_anchor, "bind")
        .bind_mount(&shared.root_location(), false)
        .unwrap();
    bound.set_private();

    let cloned = source.clone_namespace();
    let retired_anchor = Arc::downgrade(cloned.namespace().root_mount());
    let retained =
        PathHandle::acquire(cloned.namespace().root_mount().children()[0].root_location()).unwrap();
    let (created, retirement) = cloned.into_parts();
    drop((created.into_namespace(), retirement));
    let retired_mount = retained.location().mountpoint();

    // The retired path still reads the real backend, but is not a publication
    // target. A rejected preconstructed mount must release its actual lease
    // outside topology, even while the input filesystem itself stays alive.
    assert_payload(retained.location());
    let prepared = RetirementCounts::default();
    let prepared_fs = prepared.filesystem(&outer);
    assert!(
        matches!(
            retained.location().mount(&prepared_fs),
            Err(VfsError::InvalidInput)
        ),
        "a retained PH must not create a new ownerless mount cycle"
    );
    assert_eq!(prepared.lease.load(Ordering::Acquire), 1);
    assert!(matches!(
        retained
            .location()
            .bind_mount(&shared.root_location(), false),
        Err(VfsError::InvalidInput)
    ));

    let movable = directory(&bind_anchor, "movable")
        .mount(&mock_filesystem())
        .unwrap();
    assert_eq!(
        movable.move_to(retained.location()),
        Err(VfsError::InvalidInput)
    );
    assert!(Arc::ptr_eq(
        movable.location().unwrap().mountpoint(),
        &bind_anchor
    ));
    let detached = Mountpoint::new_root(&mock_filesystem());
    assert_eq!(
        detached.attach_detached(retained.location()),
        Err(VfsError::InvalidInput)
    );
    // A fresh unadopted tree is not retired merely because it is parentless.
    detached
        .attach_detached(&directory(&bind_anchor, "fresh"))
        .unwrap();
    assert_eq!(
        retired_mount.attach_detached(&directory(&bind_anchor, "revive")),
        Err(VfsError::InvalidInput)
    );
    assert_eq!(
        retired_mount.move_to(&directory(&bind_anchor, "revive-move")),
        Err(VfsError::InvalidInput)
    );
    assert_eq!(
        retired_mount.pivot_mount(&movable, &movable.root_location()),
        Err(VfsError::InvalidInput)
    );
    assert_eq!(
        movable.pivot_mount(retired_mount, retained.location()),
        Err(VfsError::InvalidInput)
    );
    assert!(retired_anchor.upgrade().is_none());
    assert!(retired_mount.location().is_none());
    assert!(retired_mount.children().is_empty());
    assert_eq!(retired_mount.active_uses.lock().users, 1);
    assert!(!retired_mount.active_uses.lock().normally_unmounted);

    // Deriving a NEW node into a live namespace remains legal; it neither
    // revives the old node nor inherits that node's publication shutdown.
    let derived = directory(&bind_anchor, "derived")
        .bind_mount(retained.location(), false)
        .unwrap();
    assert!(!Arc::ptr_eq(&derived, retired_mount));
    let derived_child = derived.root_location().mount(&mock_filesystem()).unwrap();
    derived.detach().unwrap();
    assert!(derived_child.location().is_none());
    derived
        .attach_detached(&directory(&bind_anchor, "reattached"))
        .unwrap();
    derived_child
        .attach_detached(&derived.root_location())
        .unwrap();
    assert!(derived_child.location().is_some());
    assert_payload(&derived.root_location());

    // A later real propagation event still reaches the live peer and slave,
    // but must not repopulate the retained retired receiver.
    let event = shared.root_location().mount(&prepared_fs).unwrap();
    assert!(retired_mount.children().is_empty());
    for (mount, parent) in [
        (&shared, &source_anchor),
        (&peer, &peer_anchor),
        (&slave, &slave_anchor),
        (&bound, &bind_anchor),
    ] {
        assert!(Arc::ptr_eq(mount.location().unwrap().mountpoint(), parent));
        assert_payload(&mount.root_location());
    }
    assert_eq!(peer.children().len(), 1);
    assert_eq!(slave.children().len(), 1);
    assert!(bound.children().is_empty());
    assert!(peer.is_shared());
    assert!(slave.is_slave());
    assert_eq!(shared.peer_group_id(), peer.peer_group_id());

    // Namespace destruction is local, not a propagated ordinary unmount.
    drop(source);
    assert!(event.location().is_none());
    assert!(shared.children().is_empty());
    assert!(peer.children()[0].location().is_some());
    assert!(slave.children()[0].location().is_some());
    assert_payload(&peer.root_location().resolve_mountpoint());
    assert_payload(&slave.root_location().resolve_mountpoint());
    assert_payload(&bound.root_location());
    assert_payload(&derived.root_location());
    assert_eq!(contents.flushes.load(Ordering::Acquire), 0);
    assert_eq!(prepared.flushes.load(Ordering::Acquire), 0);
    drop((peer_namespace, slave_namespace, bind_namespace));
    assert!(derived.location().is_none());
    assert!(derived_child.location().is_none());
    assert_eq!(retired_mount.active_uses.lock().users, 1);
}
