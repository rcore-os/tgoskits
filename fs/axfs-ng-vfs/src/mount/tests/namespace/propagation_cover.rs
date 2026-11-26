use super::*;

/// Own the synthetic lookup namespace while delegating its data to the existing
/// retirement backend. Every exposed entry reports this wrapper as its owner.
struct LookupFilesystem {
    self_ref: Weak<Self>,
    inner: Filesystem,
    backing_root: DirEntry,
}

impl LookupFilesystem {
    fn new(inner: Filesystem) -> Arc<Self> {
        let backing_root = inner.root_dir();
        Arc::new_cyclic(|self_ref| Self {
            self_ref: self_ref.clone(),
            inner,
            backing_root,
        })
    }
}

struct LookupLease {
    // Release the wrapper's backing references before the inner final lease.
    _owner: Arc<LookupFilesystem>,
    _inner: Option<Arc<dyn crate::FilesystemMountLease>>,
}

impl core::fmt::Debug for LookupLease {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("LookupLease")
            .finish_non_exhaustive()
    }
}

impl crate::FilesystemMountLease for LookupLease {}

impl FilesystemOps for LookupFilesystem {
    fn name(&self) -> &str {
        "cover-order-lookup-fixture"
    }

    fn root_dir(&self) -> DirEntry {
        let filesystem = self.self_ref.upgrade().expect("lookup filesystem is alive");
        DirEntry::new_dir(
            |_| DirNode::new(Arc::new(LookupDirectory(filesystem))),
            Reference::root(),
        )
    }

    fn is_readonly(&self) -> bool {
        self.inner.is_readonly()
    }

    fn mount_lease(&self) -> Option<Arc<dyn crate::FilesystemMountLease>> {
        Some(Arc::new(LookupLease {
            _owner: self.self_ref.upgrade().expect("lookup filesystem is alive"),
            _inner: self.backing_root.filesystem().mount_lease(),
        }))
    }

    fn stat(&self) -> VfsResult<StatFs> {
        self.inner.stat()
    }

    fn flush(&self) -> VfsResult<()> {
        self.backing_root.filesystem().flush()
    }

    fn shutdown(&self) -> VfsResult<()> {
        self.inner.shutdown()
    }
}

struct LookupDirectory(Arc<LookupFilesystem>);

impl NodeOps for LookupDirectory {
    fn inode(&self) -> u64 {
        self.0.backing_root.inode()
    }

    fn metadata(&self) -> VfsResult<Metadata> {
        self.0.backing_root.metadata()
    }

    fn update_metadata(&self, _: MetadataUpdate) -> VfsResult<()> {
        Err(VfsError::ReadOnlyFilesystem)
    }

    fn filesystem(&self) -> &dyn FilesystemOps {
        self.0.as_ref()
    }

    fn sync(&self, data_only: bool) -> VfsResult<()> {
        self.0.backing_root.sync(data_only)
    }

    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }
}

impl DirNodeOps for LookupDirectory {
    fn is_cacheable(&self) -> bool {
        // This journey measures mount/lease ownership, not the independent
        // global positive-dentry retention policy.
        false
    }

    fn read_dir(&self, cursor: DirectoryCursor, sink: &mut dyn DirEntrySink) -> VfsResult<usize> {
        self.0.backing_root.as_dir()?.read_dir(cursor, sink)
    }

    fn lookup(&self, name: &str) -> VfsResult<DirEntry> {
        if name != "visible" {
            return Err(VfsError::NotFound);
        }
        Ok(DirEntry::new_dir(
            |_| DirNode::new(Arc::new(Self(self.0.clone()))),
            Reference::new(None, name.into()),
        ))
    }
}

fn only_child_is(parent: &Arc<Mountpoint>, expected: &Arc<Mountpoint>) -> bool {
    let children = parent.children();
    children.len() == 1 && Arc::ptr_eq(&children[0], expected)
}

#[derive(Debug, PartialEq, Eq)]
struct CoverOrder {
    q_child_is_xq: bool,
    xq_parent_is_q_root: bool,
    xq_child_is_c: bool,
    c_parent_is_xq_root: bool,
    c_child_is_d: bool,
    d_parent_is_c_root: bool,
    d_has_no_children: bool,
    visible_root_is_d: bool,
}

#[test]
fn propagation_preserves_preexisting_cover_order_and_complete_tree_retirement() {
    let outer = Arc::new(RawSpinLock::new(()));
    let c_counts = RetirementCounts::default();
    let d_counts = RetirementCounts::default();
    let (weak_mounts, order, lookup) = {
        let p_anchor = Mountpoint::new_root(&mock_filesystem());
        let p = p_anchor.root_location().mount(&mock_filesystem()).unwrap();
        let q_anchor = Mountpoint::new_root(&mock_filesystem());
        // Bind while P is private: P/Q share the exact root dentry, but Q does
        // not receive P's propagation until it explicitly joins later.
        let q = q_anchor
            .root_location()
            .bind_mount(&p.root_location(), false)
            .unwrap();
        let p_namespace = adopt(&p_anchor);
        let q_namespace = adopt(&q_anchor);
        p.set_shared();
        let c = q
            .root_location()
            .mount(&c_counts.filesystem(&outer))
            .unwrap();
        let d = c
            .root_location()
            .mount(&Filesystem::new(LookupFilesystem::new(
                d_counts.filesystem(&outer),
            )))
            .unwrap();
        c.set_lifetime_guard(Arc::new(c_counts.probe(&c_counts.attachment, &outer)));
        d.set_lifetime_guard(Arc::new(d_counts.probe(&d_counts.attachment, &outer)));
        q.join_shared_group(&p);
        let publication;
        let writer = outer.lock();
        let (result, retirement) = p.root_location().mount_retiring(&mock_filesystem());
        publication = retirement;
        drop(writer);
        drop(publication);
        let x = result.unwrap();
        let xq = q
            .walk_tree()
            .into_iter()
            .map(|(_, _, mount)| mount)
            .find(|mount| mount.filesystem_id() == x.filesystem_id())
            .expect("Q must receive its own propagated copy of X");
        let visible = q.root_location().resolve_mountpoint();
        let order = CoverOrder {
            q_child_is_xq: only_child_is(&q, &xq),
            xq_parent_is_q_root: xq
                .location()
                .is_some_and(|loc| loc.ptr_eq(&q.root_location())),
            xq_child_is_c: only_child_is(&xq, &c),
            c_parent_is_xq_root: c
                .location()
                .is_some_and(|loc| loc.ptr_eq(&xq.root_location())),
            c_child_is_d: only_child_is(&c, &d),
            d_parent_is_c_root: d
                .location()
                .is_some_and(|loc| loc.ptr_eq(&c.root_location())),
            d_has_no_children: d.children().is_empty(),
            visible_root_is_d: Arc::ptr_eq(visible.mountpoint(), &d),
        };
        // Query through the selected mount, not through the retained D Arc.
        // Wrong replacement/stack order makes this return NotFound on Xq.
        let lookup = visible.lookup_no_follow("visible").and_then(|location| {
            let mut entries = Vec::new();
            location.read_dir(
                DirectoryCursor::START,
                &mut |name: &[u8], inode, kind, _| {
                    entries.push((name.to_vec(), inode, kind));
                    true
                },
            )?;
            Ok((Arc::ptr_eq(location.mountpoint(), &d), entries))
        });
        drop(visible);
        let weak_mounts: Vec<_> = [&p_anchor, &q_anchor, &p, &q, &c, &d, &x, &xq]
            .into_iter()
            .map(Arc::downgrade)
            .collect();
        // No raw location, mount Arc, or lookup result keeps C/D alive for the
        // automatic-retirement measurement. Only their real topology remains.
        drop((xq, x, d, c, q, p, q_anchor, p_anchor));
        drop((p_namespace, q_namespace));
        (weak_mounts, order, lookup)
    };
    let automatically_reclaimed: Vec<_> = weak_mounts
        .iter()
        .map(|mount| mount.upgrade().is_none())
        .collect();
    let resource_retirement: Vec<_> = [&c_counts, &d_counts]
        .into_iter()
        .map(|counts| {
            (
                counts.filesystem.load(Ordering::Acquire),
                counts.lease.load(Ordering::Acquire),
                counts.attachment.load(Ordering::Acquire),
                counts.flushes.load(Ordering::Acquire),
            )
        })
        .collect();
    // The incorrect replacement can orphan the C<->D cycle. AFTER recording
    // all automatic-reclamation observations, detach its still-coherent leaf
    // solely to release the failed fixture. These later drops are not evidence.
    if let Some(d) = weak_mounts[5].upgrade()
        && d.location().is_some()
    {
        d.detach()
            .expect("clean up an orphaned fixture after measuring failure");
    }
    assert_eq!(
        order,
        CoverOrder {
            q_child_is_xq: true,
            xq_parent_is_q_root: true,
            xq_child_is_c: true,
            c_parent_is_xq_root: true,
            c_child_is_d: true,
            d_parent_is_c_root: true,
            d_has_no_children: true,
            visible_root_is_d: true,
        },
        "propagation must preserve Q -> Xq -> C -> D, not replace or cover C/D"
    );
    assert_eq!(
        lookup,
        Ok((true, vec![(b"payload".to_vec(), 42, NodeType::RegularFile)]))
    );
    assert_eq!(automatically_reclaimed, vec![true; 8]);
    assert_eq!(resource_retirement, vec![(1, 1, 1, 0); 2]);
}
