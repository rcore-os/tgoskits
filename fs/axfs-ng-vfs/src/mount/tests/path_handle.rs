use super::*;

struct UseRetirementProbe {
    mount: Weak<Mountpoint>,
    retired: Arc<AtomicUsize>,
}

impl Drop for UseRetirementProbe {
    fn drop(&mut self) {
        assert!(!current_thread_owns_mount_topology_guard());
        let mount = self
            .mount
            .upgrade()
            .expect("path use must keep mount alive");
        let state = mount
            .active_uses
            .try_lock()
            .expect("retirement must be outside the active-use lock");
        assert_eq!(
            state.users, 1,
            "retirement must precede the last use return"
        );
        self.retired.fetch_add(1, Ordering::Release);
    }
}

pub(super) fn cache_retirement_probe(
    mount: &Arc<Mountpoint>,
    filesystem: &Arc<TopologyChangingFs>,
) -> Arc<AtomicUsize> {
    let retired = Arc::new(AtomicUsize::new(0));
    let entry = DirEntry::new_dir(
        |_| {
            DirNode::new(Arc::new(TopologyChangingNode {
                filesystem: filesystem.clone(),
                inode: 1,
            }))
        },
        Reference::new(None, "retired-cache-child".into()),
    );
    entry.user_data().insert(UseRetirementProbe {
        mount: Arc::downgrade(mount),
        retired: retired.clone(),
    });
    mount
        .root
        .as_dir()
        .unwrap()
        .insert_cache("retired-cache-child".into(), entry);
    retired
}

#[test]
fn path_handle_rebinding_shares_use_and_retires_location_before_guard() {
    let mount = Mountpoint::new_root(&mock_filesystem());
    let handle = PathHandle::acquire(mount.root_location()).unwrap();
    let retired = Arc::new(AtomicUsize::new(0));
    let entry = make_dir_entry("transient");
    entry.user_data().insert(UseRetirementProbe {
        mount: Arc::downgrade(&mount),
        retired: retired.clone(),
    });
    let rebound = {
        // Neither operation may reenter this gate or allocate another use.
        let _topology = MOUNT_TOPOLOGY_MUTATION.lock();
        let cloned = handle.clone();
        let rebound = cloned.rebind(Location::new(mount.clone(), entry)).unwrap();
        drop(cloned);
        rebound
    };
    drop(handle);
    drop(rebound);
    assert_eq!(retired.load(Ordering::Acquire), 1);
    assert_eq!(mount.active_uses.lock().users, 0);
}

#[test]
fn path_use_admission_rejects_foreign_owner_and_overflow_without_leaking_use() {
    let parent = Mountpoint::new_root(&mock_filesystem());
    let target = Location::new(parent.clone(), make_dir_entry("target"));
    let ops = Arc::new_cyclic(|self_ref| TopologyChangingFs {
        self_ref: self_ref.clone(),
        topology_change: Box::new(|| Ok(())),
        flushes: AtomicUsize::new(0),
        on_drop: None,
    });
    let mounted = target.mount(&Filesystem::new(ops)).unwrap();
    // This entry really reports MOCK_FS; the mounted root reports `ops`.
    let foreign = Location::new(mounted.clone(), make_dir_entry("foreign"));
    assert!(matches!(
        PathHandle::acquire(foreign.clone()),
        Err(VfsError::CrossesDevices)
    ));
    let handle = PathHandle::acquire(mounted.root_location()).unwrap();
    assert!(matches!(
        handle.rebind(foreign.clone()),
        Err(VfsError::CrossesDevices)
    ));
    drop(handle);

    mounted.active_uses.lock().users = usize::MAX;
    assert!(matches!(
        PathHandle::acquire(mounted.root_location()),
        Err(VfsError::ValueOverflow)
    ));
    assert_eq!(mounted.active_uses.lock().users, usize::MAX);
    mounted.active_uses.lock().users = 0;
    mounted.root_location().unmount().unwrap();
    assert!(matches!(
        PathHandle::acquire(foreign),
        Err(VfsError::CrossesDevices)
    ));
    assert!(matches!(
        PathHandle::acquire(mounted.root_location()),
        Err(VfsError::NotFound)
    ));
}

#[derive(Clone, Copy, Debug)]
enum UnmountFailure {
    Planning,
    Flush,
    PostFlush,
    Abandon,
}

#[test]
fn path_unmount_errors_and_abandonment_preserve_attachment_and_retire_owners() {
    for failure in [
        UnmountFailure::Planning,
        UnmountFailure::Flush,
        UnmountFailure::PostFlush,
        UnmountFailure::Abandon,
    ] {
        let parent = Mountpoint::new_root(&mock_filesystem());
        let target = Location::new(parent.clone(), make_dir_entry("target"));
        let mount_slot = Arc::new(RawSpinLock::new(Weak::<Mountpoint>::new()));
        let external_use = Arc::new(RawSpinLock::new(None));
        let retired = Arc::new(AtomicUsize::new(0));
        let flush_mount = mount_slot.clone();
        let flush_external = external_use.clone();
        let retired_mount = mount_slot.clone();
        let retired_filesystem = retired.clone();
        let ops = Arc::new_cyclic(|self_ref| TopologyChangingFs {
            self_ref: self_ref.clone(),
            topology_change: Box::new(move || {
                assert!(!current_thread_owns_mount_topology_guard());
                let mount = flush_mount.lock().upgrade().unwrap();
                {
                    let state = mount
                        .active_uses
                        .try_lock()
                        .expect("flush must run outside the active-use lock");
                    assert_eq!(state.users, 1, "target must keep its use through flush");
                }
                match failure {
                    UnmountFailure::Flush => Err(VfsError::Io),
                    UnmountFailure::PostFlush => {
                        *flush_external.lock() = Some(mount.acquire_use().unwrap());
                        Ok(())
                    }
                    UnmountFailure::Planning | UnmountFailure::Abandon => {
                        panic!("planning failure and abandonment must not flush")
                    }
                }
            }),
            flushes: AtomicUsize::new(0),
            on_drop: Some(Box::new(move || {
                assert!(!current_thread_owns_mount_topology_guard());
                assert!(
                    retired_mount.lock().upgrade().is_none(),
                    "actual filesystem owner must retire after the last mount owner"
                );
                retired_filesystem.fetch_add(1, Ordering::Release);
            })),
        });
        let fs = Filesystem::new(ops.clone());
        let mounted = target.mount(&fs).unwrap();
        *mount_slot.lock() = Arc::downgrade(&mounted);
        let handle = PathHandle::acquire(mounted.root_location()).unwrap();
        let child = if matches!(failure, UnmountFailure::Planning) {
            Some(mounted.root_location().mount(&mock_filesystem()).unwrap())
        } else {
            None
        };

        match failure {
            UnmountFailure::Planning => assert!(matches!(
                handle.prepare_unmount(),
                Err(VfsError::ResourceBusy)
            )),
            UnmountFailure::Abandon => drop(handle.prepare_unmount().unwrap()),
            UnmountFailure::Flush => {
                assert_eq!(
                    handle.prepare_unmount().unwrap().commit(),
                    Err(VfsError::Io)
                );
            }
            UnmountFailure::PostFlush => {
                assert_eq!(
                    handle.prepare_unmount().unwrap().commit(),
                    Err(VfsError::ResourceBusy)
                );
                // The independent use acquired by real flush remains busy.
                assert_eq!(
                    mounted.root_location().unmount(),
                    Err(VfsError::ResourceBusy)
                );
                drop(external_use.lock().take());
            }
        }
        assert!(mounted.location().is_some(), "{failure:?}");
        assert!(parent.children().iter().any(|m| Arc::ptr_eq(m, &mounted)));
        assert_eq!(
            ops.flushes.load(Ordering::Acquire),
            usize::from(matches!(
                failure,
                UnmountFailure::Flush | UnmountFailure::PostFlush
            ))
        );
        if let Some(child) = child {
            child.detach().unwrap();
        }
        // A plain plan must succeed after failure: the bridge leaked no use.
        drop(mounted.plan_unmount(UnmountKind::Normal).unwrap());
        let last_owner = PathHandle::acquire(mounted.root_location()).unwrap();
        mounted.detach().unwrap();
        drop(mounted);
        drop(fs);
        drop(ops);
        assert_eq!(retired.load(Ordering::Acquire), 0);
        // No filesystem, raw location, or mount Arc remains outside this path.
        // Its final guard release really retires the filesystem owner.
        drop(last_owner);
        assert_eq!(retired.load(Ordering::Acquire), 1);
    }
}
