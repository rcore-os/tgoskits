//! Read roles share real FS admissions without acquiring exact-mount writes.

use super::*;

#[test]
fn namespace_admissions_order_real_receivers_once_and_release_partial_acquisitions() {
    use crate::{MetadataChange, MutationCredentials, MutationRetire, NamespaceAdmissions};

    struct GraphOwner {
        probe: Arc<OperationProbe>,
        dropped: Arc<AtomicUsize>,
    }
    impl Drop for GraphOwner {
        fn drop(&mut self) {
            let events = self.probe.events.lock().unwrap();
            assert_eq!(
                events.iter().filter(|(_, enter)| *enter).count(),
                events.iter().filter(|(_, enter)| !*enter).count(),
                "graph owner was destroyed before all admissions exited"
            );
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    let probe = Arc::new(OperationProbe::default());
    let first = probed_layer(0, vec![], Some(probe.clone()));
    let second = probed_layer(0, vec![], Some(probe.clone()));
    let address = |location: &Location| {
        location.filesystem() as *const dyn FilesystemOps as *const () as usize
    };
    let (higher, lower) = if address(&first) > address(&second) {
        (first, second)
    } else {
        (second, first)
    };
    let host = Mountpoint::new_root(&mock_filesystem());
    let bind = |name| {
        let target = Location::new(
            host.clone(),
            make_child_dir_entry(Some(host.root.clone()), name),
        );
        let alias = target.bind_mount(&lower, false).unwrap().root_location();
        // Remove host topology ownership for the final last-owner checks.
        // These remain real bind mounts sharing the source's FS write state.
        alias.detach_mount().unwrap();
        alias
    };
    let alias = bind("rw-alias");
    let read_alias = bind("ro-alias");
    read_alias.mountpoint().set_readonly(true).unwrap();
    let read_only = probed_layer(0, vec![], Some(probe.clone()));
    read_only.mountpoint().set_readonly(true).unwrap();
    let visible = read_layer(
        1,
        vec![read_only.clone(), read_alias.clone()],
        vec![higher.clone(), alias.clone(), lower.clone()],
        Some(probe.clone()),
    );
    let write = PreparedWrite::acquire(&visible).unwrap();
    assert!(probe.events.lock().unwrap().is_empty());
    for location in [&visible, &higher, &alias, &lower] {
        assert_eq!(location.mountpoint().writes.lock().writers, 1);
    }
    for location in [&read_only, &read_alias] {
        assert_eq!(location.mountpoint().writes.lock().writers, 0);
        assert_eq!(
            write.write_for(location).unwrap_err(),
            VfsError::CrossesDevices
        );
    }
    assert_eq!(lower.mountpoint().filesystem_state.writes.lock().writers, 2);

    let retired = MutationRetire::new();
    let admissions = NamespaceAdmissions::acquire(&write).unwrap();
    let entered: Vec<_> = probe.events.lock().unwrap().clone();
    for parent in [&read_only, &read_alias] {
        let provider = parent.entry().downcast::<WriteLayerRoot>().unwrap();
        let child = provider.filesystem.root_dir();
        let weak_child = child.downgrade();
        *provider.filesystem.lookup_response.lock().unwrap() =
            Some(crate::NamespaceRead::Entry(Some(child)));
        let admission = admissions.for_location(parent).unwrap();
        let selected = admission.lookup_backing(parent, "child", &retired).unwrap();
        assert!(Arc::ptr_eq(selected.mountpoint(), parent.mountpoint()));
        assert!(selected.entry().ptr_eq(&weak_child.upgrade().unwrap()));
        assert_eq!(
            write.write_for(&selected).unwrap_err(),
            VfsError::CrossesDevices
        );
        *provider.filesystem.lookup_response.lock().unwrap() =
            Some(crate::NamespaceRead::Entry(None));
        assert!(matches!(
            admission.lookup_backing(parent, "missing", &retired),
            Err(VfsError::NotFound)
        ));
    }
    assert_eq!(
        *probe.events.lock().unwrap(),
        entered,
        "backing lookup must borrow the prepared admission without reentering"
    );
    let mut expected: Vec<_> = [&visible, &higher, &lower, &read_only]
        .into_iter()
        .map(address)
        .collect();
    expected.sort_unstable();
    assert_eq!(
        entered,
        expected
            .iter()
            .map(|owner| (*owner, true))
            .collect::<Vec<_>>(),
        "read/write aliases must enter each actual FS once, in address order"
    );
    drop(admissions);
    drop(retired);
    let events = core::mem::take(&mut *probe.events.lock().unwrap());
    assert_eq!(events.len(), expected.len() * 2);
    for owner in &expected {
        assert_eq!(
            events
                .iter()
                .filter(|event| **event == (*owner, false))
                .count(),
            1
        );
    }

    // Fail the last enter after acquiring several distinct real receivers.
    // All earlier guards must exit, but PreparedWrite must remain usable.
    probe
        .fail_owner
        .store(*expected.last().unwrap(), Ordering::Relaxed);
    assert!(matches!(
        NamespaceAdmissions::acquire(&write),
        Err(VfsError::Io)
    ));
    let events = core::mem::take(&mut *probe.events.lock().unwrap());
    assert_eq!(events.len(), (expected.len() - 1) * 2);
    for owner in &expected[..expected.len() - 1] {
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
    for location in [&visible, &higher, &alias, &lower] {
        assert_eq!(location.mountpoint().writes.lock().writers, 1);
        write
            .write_for(location)
            .unwrap()
            .check_location(location)
            .unwrap();
    }
    for location in [&read_only, &read_alias] {
        assert_eq!(location.mountpoint().writes.lock().writers, 0);
        assert!(location.is_readonly());
    }

    // Recover admission and carry a real failed completion through the outer
    // pressure/exit/retire/finish sequence without any external Location roots.
    probe.fail_owner.store(0, Ordering::Relaxed);
    let dropped = Arc::new(AtomicUsize::new(0));
    for location in [&visible, &read_only] {
        location.user_data().insert(GraphOwner {
            probe: probe.clone(),
            dropped: dropped.clone(),
        });
    }
    let locations = [&visible, &higher, &lower, &alias, &read_alias, &read_only];
    let mounts: Vec<_> = locations
        .iter()
        .map(|location| Arc::downgrade(location.mountpoint()))
        .collect();
    let filesystems: Vec<_> = locations
        .iter()
        .map(|location| {
            Arc::downgrade(
                &location
                    .entry()
                    .downcast::<WriteLayerRoot>()
                    .unwrap()
                    .filesystem,
            )
        })
        .collect();
    let assert_graph_alive = || {
        assert!(mounts.iter().all(|mount| mount.upgrade().is_some()));
        assert!(
            filesystems
                .iter()
                .all(|filesystem| filesystem.upgrade().is_some())
        );
        assert_eq!(dropped.load(Ordering::Relaxed), 0);
    };
    let scope = crate::NamespaceOperationScope::<Metadata>::acquire(&write).unwrap();
    let context = scope.context();
    let completion = scope
        .change_metadata(
            &context,
            &visible,
            MetadataChange::Chown {
                uid: None,
                gid: None,
            },
            &MutationCredentials::root(),
        )
        .unwrap()
        .unwrap();
    assert_eq!(completion.state(), crate::NamespaceState::Unchanged);
    assert!(completion.observation().is_none());
    drop((visible, higher, lower, alias, read_alias, read_only, host));
    assert_graph_alive();
    struct BeforeFinish(Option<alloc::boxed::Box<dyn FnOnce()>>);
    impl Drop for BeforeFinish {
        fn drop(&mut self) {
            self.0.take().unwrap()();
        }
    }
    let retained_mounts = mounts.clone();
    let retained_filesystems = filesystems.clone();
    let retained_dropped = dropped.clone();
    let retained_probe = probe.clone();
    context
        .retired()
        .retire(BeforeFinish(Some(alloc::boxed::Box::new(move || {
            let events = retained_probe.events.lock().unwrap();
            assert_eq!(
                events.iter().filter(|(_, enter)| *enter).count(),
                events.iter().filter(|(_, enter)| !*enter).count()
            );
            assert!(
                retained_mounts
                    .iter()
                    .all(|mount| mount.upgrade().is_some())
            );
            assert!(
                retained_filesystems
                    .iter()
                    .all(|filesystem| filesystem.upgrade().is_some())
            );
            assert_eq!(retained_dropped.load(Ordering::Relaxed), 0);
        }))));
    let outcome = scope
        .finish()
        .unwrap_or_else(|_| panic!("fixed metadata owner"));
    assert_eq!(outcome.into_result(), Err(VfsError::OperationNotSupported));
    assert_graph_alive();
    // Immutable backing roots themselves own Locations. This proves retention
    // by PreparedWrite as a whole, not that a read pin is the sole strong owner.
    drop(write);
    assert!(mounts.iter().all(|mount| mount.upgrade().is_none()));
    assert!(
        filesystems
            .iter()
            .all(|filesystem| filesystem.upgrade().is_none())
    );
    assert_eq!(dropped.load(Ordering::Relaxed), 2);
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
        let alias = target.bind_mount(&leaf, false).unwrap().root_location();
        alias.detach_mount().unwrap();
        alias
    };
    let first = bind("first");
    let second = bind("second");
    let uncovered = bind("uncovered");
    let middle = layer(1, vec![first.clone(), second.clone()]);
    let read_leaf = layer(0, vec![]);
    let read_middle = layer(1, vec![read_leaf.clone()]);
    for location in [&read_middle, &read_leaf] {
        location.mountpoint().set_readonly(true).unwrap();
    }
    let probe = Arc::new(OperationProbe::default());
    let visible = read_layer(
        2,
        vec![read_middle.clone(), middle.clone()],
        vec![middle.clone(), first.clone()],
        Some(probe.clone()),
    );
    let held = [&visible, &middle, &first, &second];
    let reads = [&read_middle, &read_leaf];

    let prepared = PreparedWrite::acquire(&visible).unwrap();
    assert!(probe.events.lock().unwrap().is_empty());
    for location in held {
        prepared
            .write_for(location)
            .unwrap()
            .check_location(location)
            .unwrap();
        // Read dependencies are traversed first: upgrading middle to Write
        // must revisit its children, including second (not a direct root).
        assert_eq!(location.mountpoint().writes.lock().writers, 1);
        assert_eq!(
            location.mountpoint().set_readonly(true),
            Err(VfsError::ResourceBusy)
        );
        assert!(!location.is_readonly());
    }
    for location in reads {
        // The Read role must demote read_middle's declared write edge too.
        assert_eq!(
            prepared.write_for(location).unwrap_err(),
            VfsError::CrossesDevices
        );
        assert_eq!(location.mountpoint().writes.lock().writers, 0);
        assert_eq!(
            location.mountpoint().filesystem_state.writes.lock().writers,
            0
        );
        location.mountpoint().set_readonly(false).unwrap();
        location.mountpoint().set_readonly(true).unwrap();
        location.mountpoint().set_filesystem_readonly(true).unwrap();
    }
    // Zero writers alone would also pass if recursion silently skipped these
    // dependencies. Their real admissions prove the complete Read subgraph.
    let admissions = crate::NamespaceAdmissions::acquire(&prepared).unwrap();
    for location in reads {
        admissions.for_location(location).unwrap();
    }
    drop(admissions);
    let owner = visible.filesystem() as *const dyn FilesystemOps as *const () as usize;
    assert_eq!(
        core::mem::take(&mut *probe.events.lock().unwrap()),
        vec![(owner, true), (owner, false)]
    );
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
    uncovered.mountpoint().set_readonly(true).unwrap();
    assert_eq!(
        PreparedWrite::acquire(&uncovered).unwrap_err(),
        VfsError::ReadOnlyFilesystem
    );
    drop(prepared);
    for location in held {
        assert_eq!(location.mountpoint().writes.lock().writers, 0);
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
        for location in held {
            assert_eq!(location.mountpoint().writes.lock().writers, 0);
            location.mountpoint().set_readonly(true).unwrap();
            location.mountpoint().set_filesystem_readonly(true).unwrap();
        }
        for location in reads {
            assert_eq!(location.mountpoint().writes.lock().writers, 0);
            assert!(location.is_readonly());
        }
    }
    assert!(
        probe.events.lock().unwrap().is_empty(),
        "prepare must not enter backend admission"
    );

    // The leaf is reached legally before an illegal 0 -> 0 Read edge points
    // to that same exact mount. Deduplication must not hide the invalid edge.
    let seen = layer(0, vec![]);
    let invalid_parent = read_layer(0, vec![seen.clone()], vec![], None);
    let read_mounts = [
        Arc::downgrade(seen.mountpoint()),
        Arc::downgrade(invalid_parent.mountpoint()),
    ];
    let read_filesystems = [&seen, &invalid_parent].map(|location| {
        Arc::downgrade(
            &location
                .entry()
                .downcast::<WriteLayerRoot>()
                .unwrap()
                .filesystem,
        )
    });
    let duplicate_read = read_layer(1, vec![seen.clone(), invalid_parent.clone()], vec![], None);
    for invalid in [
        layer(3, vec![leaf]),
        layer(1, vec![layer(1, vec![])]),
        read_layer(1, vec![layer(1, vec![])], vec![], None),
        read_layer(2, vec![layer(3, vec![])], vec![], None),
        duplicate_read,
    ] {
        assert_eq!(
            PreparedWrite::acquire(&invalid).unwrap_err(),
            VfsError::InvalidInput
        );
        assert_eq!(invalid.mountpoint().writes.lock().writers, 0);
        invalid.mountpoint().set_readonly(true).unwrap();
        invalid.mountpoint().set_filesystem_readonly(true).unwrap();
    }
    for location in [&seen, &invalid_parent] {
        assert_eq!(location.mountpoint().writes.lock().writers, 0);
        location.mountpoint().set_readonly(true).unwrap();
        location.mountpoint().set_filesystem_readonly(true).unwrap();
    }
    drop((seen, invalid_parent));
    assert!(read_mounts.iter().all(|mount| mount.upgrade().is_none()));
    assert!(
        read_filesystems
            .iter()
            .all(|filesystem| filesystem.upgrade().is_none()),
        "failed preparation leaked a read pin"
    );
}
